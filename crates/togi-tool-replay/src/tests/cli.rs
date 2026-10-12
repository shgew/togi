use clap::Parser;
use clap::error::ErrorKind;
use jiff::SignedDuration;
use jiff::tz::TimeZone;
use tempfile::tempdir;
use togi_watch::screen::View;
use togi_watchtest::{Scenario, install};

use crate::{Cli, Error, Frame, draw, prefix, run};

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(
        ["togi-tool-replay", "--state-dir", "state"]
            .into_iter()
            .chain(args.iter().copied()),
    )
}

#[test]
fn the_defaults_play_from_the_first_event_at_300_times() {
    let cli = parse(&[]).unwrap();
    assert_eq!(cli.speed, "300".parse().unwrap());
    assert_eq!((cli.from, cli.at), (1, None));
    assert_eq!((cli.width, cli.height), (240, 67));
    assert_eq!((cli.after, cli.color), (None, false));
}

#[test]
fn an_invalid_speed_is_rejected_when_the_arguments_are_read_before_any_journal_is() {
    for speed in [
        "NaN", "inf", "+inf", "-inf", "Infinity", "0", "-0", "-1", "1e999",
    ] {
        let error = parse(&[&format!("--speed={speed}")]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ValueValidation, "{speed}");
        assert!(
            error
                .to_string()
                .contains("--speed must be finite and positive"),
            "{speed}: {error}"
        );
    }
    let error = parse(&["--speed", "-1"]).unwrap_err();
    assert!(error.to_string().contains("finite and positive"), "{error}");
}

#[test]
fn the_state_directory_is_required_and_nothing_else_may_follow() {
    assert!(Cli::try_parse_from(["togi-tool-replay"]).is_err());
    assert!(parse(&["extra"]).is_err());
}

#[test]
fn the_frame_options_need_a_frame_to_apply_to() {
    for option in [
        &["--after", "40s"][..],
        &["--width", "80"],
        &["--height", "24"],
        &["--color"],
        &["--view", "log"],
    ] {
        assert_eq!(
            parse(option).unwrap_err().kind(),
            ErrorKind::MissingRequiredArgument,
            "{option:?}"
        );
        let mut with_at = vec!["--at", "2"];
        with_at.extend_from_slice(option);
        parse(&with_at).unwrap();
    }
    assert_eq!(
        parse(&["--from", "2", "--at", "3"]).unwrap_err().kind(),
        ErrorKind::ArgumentConflict
    );
}

#[test]
fn a_frame_size_view_and_delay_are_checked_when_the_arguments_are_read() {
    for bad in [
        &["--width", "0"][..],
        &["--height", "0"],
        &["--width", "70000"],
        &["--view", "nope"],
        &["--after", "soon"],
    ] {
        let mut args = vec!["--at", "2"];
        args.extend_from_slice(bad);
        assert!(parse(&args).is_err(), "{bad:?}");
    }
    let cli = parse(&["--at", "2", "--after", "1m30s", "--view", "help"]).unwrap();
    assert_eq!(cli.after, Some(SignedDuration::from_secs(90)));
    let cli = parse(&["--at", "2", "--after=-40s"]).unwrap();
    assert_eq!(cli.after, Some(SignedDuration::from_secs(-40)));
}

fn state_dir(scenario: Scenario) -> (tempfile::TempDir, Vec<String>) {
    let dir = tempdir().unwrap();
    install(dir.path(), scenario).unwrap();
    let path = dir.path().to_str().unwrap().to_owned();
    (
        dir,
        vec!["togi-tool-replay".into(), "--state-dir".into(), path],
    )
}

#[test]
fn at_prints_one_frame_of_the_recording_in_the_system_time_zone() {
    let (dir, mut args) = state_dir(Scenario::Checking);
    let events = Scenario::Checking.events().unwrap();
    let at = events[events.len() / 2].seq.get();
    args.extend(
        [
            "--at",
            &at.to_string(),
            "--after",
            "40s",
            "--view",
            "log",
            "--width",
            "100",
            "--height",
            "30",
        ]
        .map(String::from),
    );
    let mut out = Vec::new();
    run(&Cli::try_parse_from(&args).unwrap(), &mut out).unwrap();
    let want = draw(
        &events,
        prefix(&events, at),
        &Frame {
            view: View::Log,
            width: 100,
            height: 30,
            colour: false,
            after: SignedDuration::from_secs(40),
        },
        &TimeZone::system(),
    )
    .unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), want);
    drop(dir);
}

#[test]
fn every_page_can_be_printed() {
    let (_dir, args) = state_dir(Scenario::Checking);
    let events = Scenario::Checking.events().unwrap();
    for (name, view) in [
        ("main", View::Main),
        ("help", View::Help),
        ("log", View::Log),
    ] {
        let mut args = args.clone();
        args.extend(["--at", "30", "--view", name].map(String::from));
        let mut out = Vec::new();
        run(&Cli::try_parse_from(&args).unwrap(), &mut out).unwrap();
        let frame = Frame {
            view,
            width: 240,
            height: 67,
            colour: false,
            after: SignedDuration::ZERO,
        };
        let want = draw(&events, prefix(&events, 30), &frame, &TimeZone::system()).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), want, "{name}");
    }
}

#[test]
fn at_a_sequence_number_beyond_the_recording_prints_its_last_frame() {
    let (_dir, mut args) = state_dir(Scenario::Checking);
    args.extend(["--at", "99999999"].map(String::from));
    let mut out = Vec::new();
    run(&Cli::try_parse_from(&args).unwrap(), &mut out).unwrap();
    assert!(!out.is_empty());
}

struct Broken;

impl std::io::Write for Broken {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("closed"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn run_reports_an_unreadable_recording_and_a_failed_write() {
    let dir = tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let cli = Cli::try_parse_from(["togi-tool-replay", "--state-dir", path, "--at", "1"]).unwrap();
    assert!(matches!(run(&cli, &mut Vec::new()), Err(Error::Read(_))));

    let (_dir, mut args) = state_dir(Scenario::Checking);
    args.extend(["--at", "1"].map(String::from));
    let error = run(&Cli::try_parse_from(&args).unwrap(), &mut Broken).unwrap_err();
    assert_eq!(error.to_string(), "write the frame: closed");
}
