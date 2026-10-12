use std::io::{self, Write};

use clap::CommandFactory;

use super::{BANNER, Cli, examples, run};
use crate::exit::Exit;

struct Run {
    exit: Exit,
    out: String,
    err: String,
}

fn togi(args: &[&str]) -> Run {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = run(
        std::iter::once("togi").chain(args.iter().copied()),
        &mut out,
        &mut err,
    );
    Run {
        exit,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

struct Broken;

impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("closed"))
    }
}

#[test]
fn the_definition_is_consistent() {
    Cli::command().debug_assert();
}

#[test]
fn version_is_the_togi_version_on_stdout_only() {
    for flag in ["--version", "-V"] {
        let ran = togi(&[flag]);
        assert_eq!(ran.exit, Exit::Ok);
        assert_eq!(ran.out, format!("togi {}\n", togi_version::string()));
        assert_eq!(ran.err, "");
    }
}

#[test]
fn version_after_a_command_is_a_usage_error() {
    let ran = togi(&["completion", "bash", "--version"]);
    assert_eq!(ran.exit, Exit::Usage);
    assert_eq!(ran.out, "");
    assert!(
        ran.err.contains("unexpected argument '--version'"),
        "{}",
        ran.err
    );
}

#[test]
fn help_goes_to_stdout_and_exits_0() {
    for args in [
        &["--help"][..],
        &["-h"],
        &["help"],
        &["completion", "--help"],
        &["help", "completion"],
    ] {
        let ran = togi(args);
        assert_eq!(ran.exit, Exit::Ok, "{args:?}");
        assert!(ran.out.contains("Usage: togi"), "{args:?}: {}", ran.out);
        assert_eq!(ran.err, "", "{args:?}");
    }
}

#[test]
fn no_command_prints_the_wordmark_and_usage_to_stderr_and_exits_2() {
    let ran = togi(&[]);
    assert_eq!(ran.exit, Exit::Usage);
    assert_eq!(ran.out, "");
    assert!(ran.err.starts_with(BANNER), "{}", ran.err);
    assert!(ran.err.contains("Usage: togi"), "{}", ran.err);
}

#[test]
fn usage_errors_exit_2_with_stderr_only() {
    for (args, text) in [
        (&["bogus"][..], "unrecognized subcommand 'bogus'"),
        (&["--bogus"], "unexpected argument '--bogus'"),
        (
            &["--state-dir"],
            "a value is required for '--state-dir <path>'",
        ),
        (&["completion"], "required arguments were not provided"),
        (&["completion", "tcsh"], "invalid value 'tcsh'"),
        (&["completion", "bash", "zsh"], "unexpected argument 'zsh'"),
        (
            &["completion", "--shell", "bash"],
            "unexpected argument '--shell'",
        ),
        (
            &["--config", "c.json", "completion", "bash"],
            "unexpected argument '--config'",
        ),
        (
            &["completion", "bash", "--config", "c.json"],
            "unexpected argument '--config'",
        ),
    ] {
        let ran = togi(args);
        assert_eq!(ran.exit, Exit::Usage, "{args:?}");
        assert_eq!(ran.out, "", "{args:?}");
        assert!(ran.err.contains(text), "{args:?}: {}", ran.err);
        assert!(ran.err.contains("--help"), "{args:?}: {}", ran.err);
    }
}

#[test]
fn a_bad_shell_lists_the_supported_ones() {
    let ran = togi(&["completion", "tcsh"]);
    assert!(
        ran.err
            .contains("[possible values: bash, zsh, fish, nushell]"),
        "{}",
        ran.err
    );
}

#[test]
fn state_dir_before_or_after_the_command_is_accepted() {
    for args in [
        &["--state-dir", "unused", "completion", "fish"][..],
        &["completion", "fish", "--state-dir", "unused"],
    ] {
        let ran = togi(args);
        assert_eq!(ran.exit, Exit::Ok, "{args:?}: {}", ran.err);
        assert!(!ran.out.is_empty());
    }
}

#[test]
fn help_that_cannot_be_written_is_an_error() {
    let mut err = Vec::new();
    assert_eq!(run(["togi", "--help"], &mut Broken, &mut err), Exit::Error);
    assert!(
        String::from_utf8(err)
            .unwrap()
            .starts_with("togi: write output: closed")
    );
}

#[test]
fn a_script_that_cannot_be_written_is_an_error_not_a_panic() {
    let mut err = Vec::new();
    assert_eq!(
        run(["togi", "completion", "bash"], &mut Broken, &mut err),
        Exit::Error
    );
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "togi: completion: write the script: closed\n"
    );
}

#[test]
fn examples_start_every_description_in_one_column() {
    let block = examples(&[
        ("a", "first"),
        ("longer command", "second"),
        ("mid", "third"),
    ]);
    let columns: Vec<usize> = block
        .lines()
        .skip(1)
        .map(|line| {
            line.find(char::is_alphabetic)
                .map_or(0, |_| line.rfind("   ").unwrap() + 3)
        })
        .collect();
    assert_eq!(block.lines().next(), Some("Examples:"));
    assert!(columns.windows(2).all(|pair| pair[0] == pair[1]), "{block}");
}

#[test]
fn every_help_examples_block_shares_one_description_column() {
    let command = Cli::command();
    let mut commands = vec![command.clone()];
    commands.extend(command.get_subcommands().cloned());
    for mut command in commands {
        let help = command.render_help().to_string();
        let Some((_, block)) = help.split_once("Examples:\n") else {
            continue;
        };
        let block = block.split("\n\n").next().unwrap();
        let columns: Vec<usize> = block
            .lines()
            .map(|line| {
                let start = line.find(|c: char| !c.is_whitespace()).unwrap();
                let gap = line[start..].find("   ").unwrap() + start;
                gap + line[gap..].find(|c: char| c != ' ').unwrap()
            })
            .collect();
        assert!(columns.windows(2).all(|pair| pair[0] == pair[1]), "{help}");
    }
}
