use clap::Parser;
use clap::error::ErrorKind;
use jiff::SignedDuration;
use jiff::tz::TimeZone;
use togi_watch::screen::View;
use togi_watchtest::Scenario;

use super::plant::{plant, recording};
use crate::{Cli, Error, Frame, draw, load, prefix, run_with};

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

fn run(cli: &Cli, bytes: &str, out: &mut impl std::io::Write) -> Vec<u8> {
    let mut err = Vec::new();
    run_with(
        cli,
        out,
        &mut err,
        |state| load::events_from(state, bytes.as_bytes()),
        || TimeZone::UTC,
    )
    .unwrap();
    err
}

#[test]
fn at_prints_one_frame_of_the_recording_in_the_supplied_time_zone() {
    let bytes = recording(Scenario::Checking);
    let events = Scenario::Checking.events().unwrap();
    let at = events[events.len() / 2].seq.get();
    let cli = parse(&[
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
    ])
    .unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let zone = TimeZone::fixed(jiff::tz::offset(9));
    run_with(
        &cli,
        &mut out,
        &mut err,
        |state| load::events_from(state, bytes.as_bytes()),
        || zone.clone(),
    )
    .unwrap();
    assert!(err.is_empty());
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
        &zone,
    )
    .unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), want);
}

#[test]
fn every_page_can_be_printed() {
    let bytes = recording(Scenario::Checking);
    let events = Scenario::Checking.events().unwrap();
    for (name, view) in [
        ("main", View::Main),
        ("help", View::Help),
        ("log", View::Log),
    ] {
        let cli = parse(&["--at", "30", "--view", name]).unwrap();
        let mut out = Vec::new();
        assert!(run(&cli, &bytes, &mut out).is_empty());
        let frame = Frame {
            view,
            width: 240,
            height: 67,
            colour: false,
            after: SignedDuration::ZERO,
        };
        let want = draw(&events, prefix(&events, 30), &frame, &TimeZone::UTC).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), want, "{name}");
    }
}

#[test]
fn at_a_sequence_number_beyond_the_recording_prints_its_last_frame() {
    let bytes = recording(Scenario::Checking);
    let cli = parse(&["--at", "99999999"]).unwrap();
    let mut out = Vec::new();
    assert!(run(&cli, &bytes, &mut out).is_empty());
    assert!(!out.is_empty());
}

#[test]
fn at_draws_a_recording_with_an_unknown_kind_and_fields_as_the_known_events_alone_and_leaves_it_as_it_is()
 {
    let mut bytes = recording(Scenario::Checking);
    let planted = plant(&mut bytes);
    let before = bytes.clone();
    let all = Scenario::Checking.events().unwrap();
    let known: Vec<_> = all
        .iter()
        .filter(|event| event.seq.get() != planted.kind_line as u64)
        .cloned()
        .collect();
    let at = all.last().unwrap().seq.get();
    let cli = parse(&["--at", &at.to_string()]).unwrap();
    let mut out = Vec::new();
    let err = String::from_utf8(run(&cli, &bytes, &mut out)).unwrap();
    assert_eq!(err.lines().count(), 1, "{err}");
    let report = err.strip_prefix("togi-tool-replay: ").unwrap();
    assert!(report.starts_with(&planted.skipped_kind()), "{err}");
    for ignored in planted.ignored_fields() {
        assert!(report.contains(&ignored), "{err}");
    }
    let frame = Frame {
        view: View::Main,
        width: 240,
        height: 67,
        colour: false,
        after: SignedDuration::ZERO,
    };
    let want = draw(&known, prefix(&known, at), &frame, &TimeZone::UTC).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), want);
    assert_eq!(bytes, before);
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
    let cli = parse(&["--at", "1"]).unwrap();
    let error = run_with(
        &cli,
        &mut Vec::new(),
        &mut Vec::new(),
        |state| load::events_from(state, std::io::BufReader::new(super::load::Missing)),
        || panic!("an unreadable recording must fail before looking up the time zone"),
    )
    .unwrap_err();
    assert!(matches!(error, Error::Read(_)));

    let bytes = recording(Scenario::Checking);
    let error = run_with(
        &cli,
        &mut Broken,
        &mut Vec::new(),
        |state| load::events_from(state, bytes.as_bytes()),
        || TimeZone::UTC,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "write the frame: closed");
}
