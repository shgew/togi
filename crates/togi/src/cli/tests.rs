use std::io::{self, Write};

use clap::CommandFactory;
use test_case::test_case;

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

#[test_case("--version" ; "long flag")]
#[test_case("-V" ; "short flag")]
fn version_is_the_togi_version_on_stdout_only(flag: &str) {
    let ran = togi(&[flag]);
    assert_eq!(ran.exit, Exit::Ok);
    assert_eq!(ran.out, format!("togi {}\n", togi_version::string()));
    assert_eq!(ran.err, "");
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

#[test_case(&["--help"] ; "long flag")]
#[test_case(&["-h"] ; "short flag")]
#[test_case(&["help"] ; "help command")]
#[test_case(&["completion", "--help"] ; "command flag")]
#[test_case(&["help", "completion"] ; "help for a command")]
fn help_goes_to_stdout_and_exits_0(args: &[&str]) {
    let ran = togi(args);
    assert_eq!(ran.exit, Exit::Ok);
    assert!(ran.out.contains("Usage: togi"), "{}", ran.out);
    assert_eq!(ran.err, "");
}

#[test]
fn no_command_prints_the_wordmark_and_usage_to_stderr_and_exits_2() {
    let ran = togi(&[]);
    assert_eq!(ran.exit, Exit::Usage);
    assert_eq!(ran.out, "");
    assert!(ran.err.starts_with(BANNER), "{}", ran.err);
    assert!(ran.err.contains("Usage: togi"), "{}", ran.err);
}

#[test_case(&["--state-dir", "/x"] ; "separate value")]
#[test_case(&["--state-dir=/x"] ; "attached value")]
fn state_dir_without_a_command_prints_the_wordmark_and_usage_to_stderr_and_exits_2(args: &[&str]) {
    let ran = togi(args);
    assert_eq!(ran.exit, Exit::Usage);
    assert_eq!(ran.out, "");
    assert!(ran.err.starts_with(BANNER), "{}", ran.err);
    assert!(ran.err.contains("Usage: togi"), "{}", ran.err);
    assert!(ran.err.contains("Commands:"), "{}", ran.err);
}

#[test_case(&["bogus"], "unrecognized subcommand 'bogus'" ; "unknown subcommand")]
#[test_case(&["--bogus"], "unexpected argument '--bogus'" ; "unknown flag")]
#[test_case(&["--state-dir"], "a value is required for '--state-dir <path>'" ; "state dir without a value")]
#[test_case(&["completion"], "required arguments were not provided" ; "completion without a shell")]
#[test_case(&["completion", "tcsh"], "invalid value 'tcsh'" ; "unknown shell")]
#[test_case(&["completion", "bash", "zsh"], "unexpected argument 'zsh'" ; "two shells")]
#[test_case(&["completion", "--shell", "bash"], "unexpected argument '--shell'" ; "shell flag")]
fn usage_errors_exit_2_with_stderr_only(args: &[&str], text: &str) {
    let ran = togi(args);
    assert_eq!(ran.exit, Exit::Usage);
    assert_eq!(ran.out, "");
    assert!(ran.err.contains(text), "{}", ran.err);
    assert!(ran.err.contains("--help"), "{}", ran.err);
}

#[test_case(&["--config", "c.json", "completion", "bash"] ; "leading")]
#[test_case(&["--config=c.json", "completion", "bash"] ; "leading attached")]
#[test_case(
    &["--state-dir", "s", "--config", "c.json", "completion", "bash"] ;
    "after a state dir"
)]
#[test_case(&["completion", "bash", "--config", "c.json"] ; "trailing")]
#[test_case(&["completion", "--config=c.json", "bash"] ; "between attached")]
#[test_case(&["completion", "--config", "c.json", "bash"] ; "between")]
#[test_case(&["--config", "c.json"] ; "without a command")]
#[test_case(&["--config=c.json"] ; "attached without a command")]
#[test_case(&["--config"] ; "without a value")]
#[test_case(&["--config", "completion"] ; "command as the value")]
#[test_case(&["--config=c.json", "--version"] ; "before version")]
#[test_case(&["--config", "c.json", "--help"] ; "before help")]
#[test_case(&["completion", "--config"] ; "trailing without a value")]
fn config_is_rejected_with_the_scope_error_wherever_it_is_written(args: &[&str]) {
    let ran = togi(args);
    assert_eq!(ran.exit, Exit::Usage);
    assert_eq!(ran.out, "");
    assert!(
        ran.err
            .starts_with("error: --config applies only to doctor, run and reset\n"),
        "{}",
        ran.err
    );
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

#[test_case(
    &["togi", "--config", "c", "doctor"],
    &["togi", "doctor", "--config", "c"] ;
    "doctor"
)]
#[test_case(
    &["togi", "--config=c", "run", "--no-tui"],
    &["togi", "run", "--config=c", "--no-tui"] ;
    "attached value"
)]
#[test_case(
    &["togi", "--state-dir", "s", "--config", "c", "reset", "--all"],
    &["togi", "--state-dir", "s", "reset", "--config", "c", "--all"] ;
    "after a state dir"
)]
#[test_case(
    &["togi", "--config", "c", "--state-dir=s", "run"],
    &["togi", "--state-dir=s", "run", "--config", "c"] ;
    "before an attached state dir"
)]
#[test_case(
    &["togi", "--config", "a", "--state-dir", "s", "--config=b", "--state-dir=t", "run", "x"],
    &["togi", "--state-dir", "s", "--state-dir=t", "run", "--config", "a", "--config=b", "x"] ;
    "repeated configs between state dirs"
)]
fn a_leading_config_moves_after_the_command_name(args: &[&str], want: &[&str]) {
    assert_eq!(moved(args), want);
}

#[test_case(&["togi"] ; "no arguments")]
#[test_case(&["togi", "doctor", "--config", "c"] ; "config already after the command")]
#[test_case(&["togi", "--config", "c"] ; "config without a command")]
#[test_case(&["togi", "--config"] ; "config without a value")]
#[test_case(&["togi", "--config", "c", "--help"] ; "config before help")]
#[test_case(&["togi", "--help", "--config", "c", "doctor"] ; "help before config")]
#[test_case(&["togi", "--state-dir", "s", "doctor"] ; "no config")]
fn other_arguments_are_left_as_they_are(args: &[&str]) {
    assert_eq!(moved(args), args);
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
fn no_arguments_at_all_are_left_as_they_are() {
    assert!(config_after_command(Vec::new()).is_empty());
}

#[test]
fn a_non_utf8_argument_is_left_as_it_is() {
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

#[test_case(&["--state-dir", "unused", "completion", "fish"] ; "before the command")]
#[test_case(&["completion", "fish", "--state-dir", "unused"] ; "after the command")]
fn state_dir_before_or_after_the_command_is_accepted(args: &[&str]) {
    let ran = togi(args);
    assert_eq!(ran.exit, Exit::Ok, "{args:?}: {}", ran.err);
    assert!(!ran.out.is_empty());
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
