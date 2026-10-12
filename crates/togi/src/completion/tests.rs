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
    assert_eq!(offering, ["run", "doctor", "reset"]);
    for &shell in Shell::value_variants() {
        let script = text(shell);
        for word in ["config", "tuning-boot", "core", "all"] {
            assert!(script.contains(word), "{shell:?} lacks {word}");
        }
    }
}

/// The list the journal accepts for `--kind`, as the scripts embed it.
fn kinds() -> String {
    let names = togi_journal::KindSelector::names();
    assert!(names.contains(&"session") && names.contains(&"session.start"));
    names.join(" ")
}

#[test]
fn bash_kind_completion_splits_on_the_last_comma_over_the_journal_kinds() {
    let script = text(Shell::Bash);
    assert!(
        script.contains(&format!(
            r#"COMPREPLY=($(compgen -P "${{prefix}}" -W "{}" -- "${{cur##*,}}"))"#,
            kinds()
        )),
        "{script}"
    );
    assert!(!script.contains(&format!(r#"-W "{}" -- "${{cur}}""#, kinds())));
}

#[test]
fn zsh_kind_completion_is_a_comma_separated_sequence_of_the_journal_kinds() {
    let script = text(Shell::Zsh);
    assert!(
        script.contains(&format!(":kinds:_sequence compadd - {}", kinds())),
        "{script}"
    );
    assert!(!script.contains(&format!(":kinds:({})", kinds())));
}

#[test]
fn fish_kind_completion_prefixes_each_kind_with_the_items_before_the_last_comma() {
    let script = text(Shell::Fish);
    assert!(
        script.contains(&format!("    for kind in {}\n", kinds())),
        "{script}"
    );
    assert!(script.contains("-l kind -d"), "{script}");
    assert!(script.contains("-a '(__togi_kinds)'"), "{script}");
    assert!(!script.contains("-a \"backend"), "{script}");
}

#[test]
fn nushell_kind_completion_lists_the_journal_kinds_as_one_value() {
    let script = text(Shell::Nushell);
    let quoted: Vec<String> = togi_journal::KindSelector::names()
        .iter()
        .map(|kind| format!("\"{kind}\""))
        .collect();
    assert!(
        script.contains(&format!("[ {} ]", quoted.join(" "))),
        "{script}"
    );
}
