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
fn every_script_completes_config_for_the_commands_that_take_it() {
    for &shell in Shell::value_variants() {
        assert!(text(shell).contains("config"), "{shell:?}");
    }
}

#[test]
fn the_top_level_command_does_not_define_config() {
    assert!(
        crate::cli::command()
            .get_arguments()
            .all(|arg| arg.get_id() != "path"),
        "--config belongs to the commands that read the configuration"
    );
}
