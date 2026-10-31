use clap::Parser;
use clap::error::ErrorKind;
use jiff::SignedDuration;
use jiff::tz::TimeZone;
use test_case::test_case;
use togi_watch::screen::{Colours, View};
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

#[test_case("NaN")]
#[test_case("inf")]
#[test_case("+inf"; "plus inf")]
#[test_case("-inf"; "minus inf")]
#[test_case("Infinity")]
#[test_case("0"; "zero")]
#[test_case("-0"; "negative zero")]
#[test_case("-1"; "negative")]
#[test_case("1e999"; "overflow")]
fn an_invalid_speed_is_rejected_when_the_arguments_are_read_before_any_journal_is(speed: &str) {
    let error = parse(&[&format!("--speed={speed}")]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ValueValidation);
    assert!(
        error
            .to_string()
            .contains("--speed must be finite and positive"),
        "{error}"
    );
}

#[test]
fn a_negative_speed_in_its_own_argument_is_rejected() {
    let error = parse(&["--speed", "-1"]).unwrap_err();
    assert!(error.to_string().contains("finite and positive"), "{error}");
}

#[test]
fn the_state_directory_is_required_and_nothing_else_may_follow() {
    assert!(Cli::try_parse_from(["togi-tool-replay"]).is_err());
    assert!(parse(&["extra"]).is_err());
}

#[test_case(&["--after", "40s"]; "after")]
#[test_case(&["--width", "80"]; "width")]
#[test_case(&["--height", "24"]; "height")]
#[test_case(&["--color"]; "color")]
#[test_case(&["--rich"]; "rich")]
#[test_case(&["--view", "log"]; "view")]
fn the_frame_options_need_a_frame_to_apply_to(option: &[&str]) {
    assert_eq!(
        parse(option).unwrap_err().kind(),
        ErrorKind::MissingRequiredArgument
    );
    let mut with_at = vec!["--at", "2"];
    with_at.extend_from_slice(option);
    parse(&with_at).unwrap();
}

#[test]
fn from_and_at_conflict() {
    assert_eq!(
        parse(&["--from", "2", "--at", "3"]).unwrap_err().kind(),
        ErrorKind::ArgumentConflict
    );
}

#[test]
fn console_and_rich_frame_profiles_are_mutually_exclusive() {
    assert_eq!(
        parse(&["--at", "2", "--color", "--rich"])
            .unwrap_err()
            .kind(),
        ErrorKind::ArgumentConflict
    );
}

#[test]
fn rich_prints_unicode_and_truecolour_from_the_recording() {
    let events = Scenario::Recovering.events().unwrap();
    let at = events.last().unwrap().seq.get().to_string();
    let bytes = recording(Scenario::Recovering);
    let cli = parse(&["--at", &at, "--rich", "--after", "40s"]).unwrap();
    let mut out = Vec::new();
    assert!(run(&cli, &bytes, &mut out).is_empty());
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains('▆'), "{text}");
    assert!(text.contains("\x1b[0;38;2;255;107;107m"), "{text}");
}

#[test_case(&["--width", "0"]; "zero width")]
#[test_case(&["--height", "0"]; "zero height")]
#[test_case(&["--width", "70000"]; "width past u16")]
#[test_case(&["--view", "nope"]; "unknown view")]
#[test_case(&["--after", "soon"]; "delay that is not a duration")]
fn a_bad_frame_size_view_or_delay_is_refused_when_the_arguments_are_read(bad: &[&str]) {
    let mut args = vec!["--at", "2"];
    args.extend_from_slice(bad);
    assert!(parse(&args).is_err());
}

#[test]
fn a_frame_delay_is_read_as_a_signed_duration() {
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
    let bytes = recording(Scenario::Mapping);
    let events = Scenario::Mapping.events().unwrap();
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
            colours: Colours::None,
            after: SignedDuration::from_secs(40),
        },
        &zone,
    )
    .unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), want);
}

#[test_case("main", View::Main; "main view")]
#[test_case("help", View::Help; "help view")]
#[test_case("log", View::Log; "log view")]
fn every_view_can_be_printed(name: &str, view: View) {
    let bytes = recording(Scenario::Nonzero);
    let events = Scenario::Nonzero.events().unwrap();
    let cli = parse(&["--at", "30", "--view", name]).unwrap();
    let mut out = Vec::new();
    assert!(run(&cli, &bytes, &mut out).is_empty());
    let frame = Frame {
        view,
        width: 240,
        height: 67,
        colours: Colours::None,
        after: SignedDuration::ZERO,
    };
    let want = draw(&events, prefix(&events, 30), &frame, &TimeZone::UTC).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), want);
}

#[test]
fn at_a_sequence_number_beyond_the_recording_prints_its_last_frame() {
    let bytes = recording(Scenario::Nonzero);
    let cli = parse(&["--at", "99999999"]).unwrap();
    let mut out = Vec::new();
    assert!(run(&cli, &bytes, &mut out).is_empty());
    assert!(!out.is_empty());
}

#[test]
fn at_draws_a_recording_with_an_unknown_kind_and_fields_as_the_known_events_alone_and_leaves_it_as_it_is()
 {
    let mut bytes = recording(Scenario::Between);
    let planted = plant(&mut bytes);
    let before = bytes.clone();
    let all = Scenario::Between.events().unwrap();
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
        colours: Colours::None,
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

    let bytes = recording(Scenario::Nonzero);
    let error = run_with(
        &cli,
        &mut Broken,
        &mut Vec::new(),
        |state| load::events_from(state, bytes.as_bytes()),
        || TimeZone::UTC,
    )
    .unwrap_err();
    assert_eq!(togi_machine::error_chain(&error), "write the frame: closed");
}
