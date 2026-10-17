use std::io::{self, Write};

use clap::CommandFactory;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use super::{BANNER, Cli, config_after_command, examples, run};
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
fn state_dir_without_a_command_prints_the_wordmark_and_usage_to_stderr_and_exits_2() {
    for args in [&["--state-dir", "/x"][..], &["--state-dir=/x"]] {
        let ran = togi(args);
        assert_eq!(ran.exit, Exit::Usage, "{args:?}");
        assert_eq!(ran.out, "", "{args:?}");
        assert!(ran.err.starts_with(BANNER), "{args:?}: {}", ran.err);
        assert!(ran.err.contains("Usage: togi"), "{args:?}: {}", ran.err);
        assert!(ran.err.contains("Commands:"), "{args:?}: {}", ran.err);
    }
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
    ] {
        let ran = togi(args);
        assert_eq!(ran.exit, Exit::Usage, "{args:?}");
        assert_eq!(ran.out, "", "{args:?}");
        assert!(ran.err.contains(text), "{args:?}: {}", ran.err);
        assert!(ran.err.contains("--help"), "{args:?}: {}", ran.err);
    }
}

#[test]
fn config_is_rejected_with_the_scope_error_wherever_it_is_written() {
    for args in [
        &["--config", "c.json", "completion", "bash"][..],
        &["--config=c.json", "completion", "bash"],
        &[
            "--state-dir",
            "s",
            "--config",
            "c.json",
            "completion",
            "bash",
        ],
        &["completion", "bash", "--config", "c.json"],
        &["completion", "--config=c.json", "bash"],
        &["completion", "--config", "c.json", "bash"],
        &["--config", "c.json"],
        &["--config=c.json"],
        &["--config"],
        &["--config", "completion"],
        &["--config=c.json", "--version"],
        &["--config", "c.json", "--help"],
        &["completion", "--config"],
    ] {
        let ran = togi(args);
        assert_eq!(ran.exit, Exit::Usage, "{args:?}");
        assert_eq!(ran.out, "", "{args:?}");
        assert!(
            ran.err
                .starts_with("error: --config applies only to doctor, run and reset\n"),
            "{args:?}: {}",
            ran.err
        );
    }
}

#[test]
fn the_scope_error_shows_the_usage_of_the_command_it_names() {
    let ran = togi(&["--config", "c.json", "completion", "bash"]);
    assert!(ran.err.contains("Usage: togi completion"), "{}", ran.err);
    assert!(
        !togi(&["--config", "c.json"])
            .err
            .contains("Usage: togi completion")
    );
    assert!(
        !togi(&["--config", "completion"])
            .err
            .contains("Usage: togi completion")
    );
}

fn moved(args: &[&str]) -> Vec<String> {
    config_after_command(args.iter().map(OsString::from).collect())
        .into_iter()
        .map(|arg| arg.into_string().unwrap())
        .collect()
}

#[test]
fn a_leading_config_moves_after_the_command_name() {
    for (args, want) in [
        (
            &["togi", "--config", "c", "doctor"][..],
            &["togi", "doctor", "--config", "c"][..],
        ),
        (
            &["togi", "--config=c", "run", "--no-tui"],
            &["togi", "run", "--config=c", "--no-tui"],
        ),
        (
            &[
                "togi",
                "--state-dir",
                "s",
                "--config",
                "c",
                "reset",
                "--all",
            ],
            &[
                "togi",
                "--state-dir",
                "s",
                "reset",
                "--config",
                "c",
                "--all",
            ],
        ),
        (
            &["togi", "--config", "c", "--state-dir=s", "run"],
            &["togi", "--state-dir=s", "run", "--config", "c"],
        ),
        (
            &[
                "togi",
                "--config",
                "a",
                "--state-dir",
                "s",
                "--config=b",
                "--state-dir=t",
                "run",
                "x",
            ],
            &[
                "togi",
                "--state-dir",
                "s",
                "--state-dir=t",
                "run",
                "--config",
                "a",
                "--config=b",
                "x",
            ],
        ),
    ] {
        assert_eq!(moved(args), want, "{args:?}");
    }
}

#[test]
fn a_leading_config_moves_after_a_non_utf8_command_name() {
    let arg = |text: &str| OsString::from(text);
    let command = OsString::from_vec(vec![0xff]);
    let args = vec![
        arg("togi"),
        arg("--config"),
        arg("c"),
        command.clone(),
        arg("x"),
    ];
    assert_eq!(
        config_after_command(args),
        [arg("togi"), command, arg("--config"), arg("c"), arg("x")]
    );
}

#[test]
fn other_arguments_are_left_as_they_are() {
    for args in [
        &["togi"][..],
        &["togi", "doctor", "--config", "c"],
        &["togi", "--config", "c"],
        &["togi", "--config"],
        &["togi", "--config", "c", "--help"],
        &["togi", "--help", "--config", "c", "doctor"],
        &["togi", "--state-dir", "s", "doctor"],
    ] {
        assert_eq!(moved(args), args, "{args:?}");
    }
    assert!(config_after_command(Vec::new()).is_empty());
    let non_utf8 = vec![
        OsString::from("togi"),
        OsString::from_vec(vec![0xff]),
        OsString::from("x"),
    ];
    assert_eq!(config_after_command(non_utf8.clone()), non_utf8);
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
            let name = command.get_name();
            assert_eq!(
                name, "help",
                "`{name}` has no Examples block in its help:\n{help}"
            );
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
