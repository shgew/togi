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
    assert_eq!(offering, ["doctor", "reset"]);
    for &shell in Shell::value_variants() {
        let script = text(shell);
        for word in ["config", "tuning-boot", "core", "all"] {
            assert!(script.contains(word), "{shell:?} lacks {word}");
        }
    }
}

#[test]
fn events_kind_completes_the_known_kinds_and_groups() {
    let root = crate::cli::command();
    let events = root.find_subcommand("events").unwrap();
    let kind = events
        .get_arguments()
        .find(|arg| arg.get_long() == Some("kind"))
        .unwrap();
    let completed: Vec<String> = kind
        .get_value_parser()
        .possible_values()
        .unwrap()
        .map(|value| value.get_name().to_owned())
        .filter(|name| name.starts_with("se"))
        .collect();
    assert!(
        completed.iter().any(|name| name == "session"),
        "{completed:?}"
    );
    assert!(
        completed.iter().any(|name| name == "session.start"),
        "{completed:?}"
    );
    for &shell in Shell::value_variants() {
        assert!(text(shell).contains("session"), "{shell:?}");
    }
}
