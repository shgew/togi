use clap::ValueEnum;

use super::{Shell, script};

fn text(shell: Shell) -> String {
    String::from_utf8(script(shell)).unwrap()
}

#[test]
fn the_shells_are_the_four_the_spec_lists() {
    let names: Vec<String> = Shell::value_variants()
        .iter()
        .map(|shell| shell.to_possible_value().unwrap().get_name().to_owned())
        .collect();
    assert_eq!(names, ["bash", "zsh", "fish", "nushell"]);
}

#[test]
fn every_script_completes_the_command_and_its_shared_flag() {
    for &shell in Shell::value_variants() {
        let script = text(shell);
        for word in ["completion", "state-dir", "togi"] {
            assert!(script.contains(word), "{shell:?} lacks {word}");
        }
    }
}

#[test]
fn only_the_commands_that_read_the_configuration_offer_it() {
    let root = crate::cli::command();
    let takes_config = |command: &clap::Command| {
        command
            .get_arguments()
            .any(|arg| arg.get_long() == Some("config"))
    };
    assert!(!takes_config(&root));
    let offering: Vec<&str> = root
        .get_subcommands()
        .filter(|command| takes_config(command))
        .map(clap::Command::get_name)
        .collect();
    assert_eq!(offering, ["run", "doctor"]);
    for &shell in Shell::value_variants() {
        let script = text(shell);
        for word in ["config", "tuning-boot", "all"] {
            assert!(script.contains(word), "{shell:?} lacks {word}");
        }
    }
}

#[test_case::test_case(Shell::Bash; "bash")]
#[test_case::test_case(Shell::Zsh; "zsh")]
#[test_case::test_case(Shell::Fish; "fish")]
#[test_case::test_case(Shell::Nushell; "nushell")]
fn internal_commands_are_not_in_the_script(shell: Shell) {
    let script = text(shell);
    assert!(!script.contains("night-end"));
    assert!(!script.contains("--kind"));
}
