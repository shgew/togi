use std::path::Path;

use clap::{Args, CommandFactory, Parser};

use crate::cli::config_after_command;

use super::{ConfigArgs, DEFAULT_CONFIG_PATH, DEFAULT_STATE_DIR, StateDirArgs};

#[derive(Debug, Parser)]
struct Top {
    #[command(flatten)]
    state_dir: StateDirArgs,
    #[command(subcommand)]
    command: Sub,
}

#[derive(Debug, clap::Subcommand)]
enum Sub {
    WithConfig(WithConfig),
    WithoutConfig,
}

#[derive(Debug, Args)]
struct WithConfig {
    #[command(flatten)]
    config: ConfigArgs,
}

fn parse(args: &[&str]) -> Result<Top, clap::Error> {
    Top::try_parse_from(std::iter::once("togi").chain(args.iter().copied()))
}

#[test]
fn the_definitions_are_consistent() {
    Top::command().debug_assert();
}

#[test]
fn state_dir_defaults_to_the_spec_directory() {
    let top = parse(&["without-config"]).unwrap();
    assert_eq!(top.state_dir.state_dir, Path::new(DEFAULT_STATE_DIR));
    assert_eq!(DEFAULT_STATE_DIR, "/var/lib/togi");
}

#[test]
fn state_dir_is_accepted_before_and_after_every_command() {
    for args in [
        ["--state-dir", "/s", "without-config"],
        ["without-config", "--state-dir", "/s"],
    ] {
        assert_eq!(
            parse(&args).unwrap().state_dir.state_dir,
            Path::new("/s"),
            "{args:?}"
        );
    }
    for args in [
        ["--state-dir", "/s", "with-config"],
        ["with-config", "--state-dir", "/s"],
    ] {
        assert_eq!(
            parse(&args).unwrap().state_dir.state_dir,
            Path::new("/s"),
            "{args:?}"
        );
    }
}

#[test]
fn config_defaults_and_reports_whether_it_was_given() {
    let Sub::WithConfig(absent) = parse(&["with-config"]).unwrap().command else {
        panic!("wrong command");
    };
    assert!(!absent.config.is_explicit());
    assert_eq!(
        absent.config.path(Path::new("/d.json")),
        Path::new("/d.json")
    );

    let Sub::WithConfig(given) = parse(&["with-config", "--config", "/c.json"])
        .unwrap()
        .command
    else {
        panic!("wrong command");
    };
    assert!(given.config.is_explicit());
    assert_eq!(
        given.config.path(Path::new("/d.json")),
        Path::new("/c.json")
    );
}

#[test]
fn config_is_rejected_where_it_is_not_attached() {
    for args in [
        &["without-config", "--config", "/c.json"][..],
        &["--config", "/c.json", "without-config"][..],
    ] {
        let error = parse(args).unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::UnknownArgument,
            "{args:?}"
        );
    }
}

#[test]
fn config_is_accepted_before_and_after_the_command_name_it_is_attached_to() {
    for args in [
        &["--config", "/c.json", "with-config"][..],
        &["--config=/c.json", "with-config"],
        &["with-config", "--config", "/c.json"],
        &["with-config", "--config=/c.json"],
        &["--state-dir", "/s", "--config", "/c.json", "with-config"],
    ] {
        let normalized = config_after_command(
            std::iter::once("togi")
                .chain(args.iter().copied())
                .map(Into::into)
                .collect(),
        );
        let top = Top::try_parse_from(normalized).unwrap();
        let Sub::WithConfig(with) = top.command else {
            panic!("wrong command")
        };
        assert!(with.config.is_explicit(), "{args:?}");
        assert_eq!(
            with.config.path(Path::new("/d.json")),
            Path::new("/c.json"),
            "{args:?}"
        );
    }
}

#[test]
fn the_config_help_names_the_default_path() {
    assert!(
        ConfigArgs::augment_args(clap::Command::new("x"))
            .render_help()
            .to_string()
            .contains(&format!("[default: {DEFAULT_CONFIG_PATH}]"))
    );
}

#[test]
fn a_missing_file_is_the_default_configuration_unless_it_was_asked_for() {
    let missing = Path::new("/config.json");
    assert_eq!(
        super::read(missing, false, |_| Err(std::io::ErrorKind::NotFound.into())).unwrap(),
        (togi_config::Config::default(), false)
    );
    assert!(matches!(
        super::read(missing, true, |_| Err(std::io::ErrorKind::NotFound.into())),
        Err(togi_config::LoadError::Read { .. })
    ));
    assert!(matches!(
        super::read(missing, false, |_| Ok("{".into())),
        Err(togi_config::LoadError::Parse { .. })
    ));
}

fn config_args(path: Option<&Path>) -> ConfigArgs {
    ConfigArgs {
        path: path.map(Path::to_path_buf),
    }
}

#[test]
fn load_reads_the_default_the_caller_names_and_the_given_file_over_it() {
    let default = Path::new("/default.json");
    let given = Path::new("/given.json");
    let read = |path: &Path| Ok(if path == default { "{" } else { "{}" }.to_owned());
    assert!(matches!(
        config_args(None).load_with_source_read(default, read),
        Err(togi_config::LoadError::Parse { .. })
    ));
    assert_eq!(
        config_args(Some(given))
            .load_with_source_read(default, read)
            .unwrap()
            .0,
        togi_config::Config::default()
    );
}

#[test]
fn a_default_that_cannot_be_read_for_another_reason_than_absence_is_an_error() {
    let default = Path::new("/config.json");
    assert!(matches!(
        config_args(None).load_with_source_read(default, |_| Err(
            std::io::ErrorKind::PermissionDenied.into()
        )),
        Err(togi_config::LoadError::Read { .. })
    ));
}

#[test]
fn load_with_source_names_the_file_read_and_whether_one_supplied_the_configuration() {
    let default = Path::new("/default.json");
    let given = Path::new("/given.json");
    let (_, source) = config_args(None)
        .load_with_source_read(default, |_| Err(std::io::ErrorKind::NotFound.into()))
        .unwrap();
    assert_eq!(
        (source.path.as_str(), source.file),
        (default.to_str().unwrap(), false)
    );
    let (_, source) = config_args(Some(given))
        .load_with_source_read(default, |_| Ok("{}".into()))
        .unwrap();
    assert_eq!(
        (source.path.as_str(), source.file),
        (given.to_str().unwrap(), true)
    );
}
