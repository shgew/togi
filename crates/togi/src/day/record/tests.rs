use std::collections::VecDeque;

use test_case::test_case;
use togi_journal::{DayBootStart, DayCheckpoint, DayRecord, DayUncleanEnd, Event, Seq};
use togi_machine::{BiosContext, EndEvidence, ObservedOffset, PerCore, ResetReason};

use super::*;

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).unwrap()
}

fn core(index: i64) -> CoreId {
    CoreId::new(index).unwrap()
}

fn boot(name: &str) -> BootId {
    name.to_owned().into()
}

fn event(seq: u64, boot_name: &str, second: i64, payload: impl Into<Payload>) -> Event {
    let payload = payload.into();
    Event {
        seq: Seq::new(seq).unwrap(),
        time: at(second),
        boot: boot(boot_name),
        mono: None,
        cause: Vec::new(),
        msg: payload.message(),
        payload,
    }
}

fn two_cores(value: i64) -> PerCore<ObservedOffset> {
    [
        (core(0), ObservedOffset(value)),
        (core(1), ObservedOffset(value)),
    ]
    .into_iter()
    .collect()
}

/// Boots `first` and `second` ended uncleanly, in that order; the second was last read back at second 700.
fn crashes(recorded: &[&str]) -> DayCrashes {
    let end = |seq: u64, crashed: &str, value: i64| {
        event(
            seq,
            "next",
            1000 + seq.cast_signed(),
            DayUncleanEnd {
                boot: boot(crashed),
                last_checkpoint: None,
                offsets: two_cores(value),
                reset_reason: ResetReason {
                    kind: None,
                    raw: Vec::new(),
                    supported: false,
                },
                evidence: EndEvidence::unknown(),
            },
        )
    };
    let start = |seq: u64, name: &str, second: i64| {
        event(
            seq,
            name,
            second,
            DayBootStart {
                build: crate::report::current_build(),
                bios: BiosContext::default(),
                offsets: two_cores(0),
            },
        )
    };
    let mut events = vec![
        start(1, "first", 100),
        start(2, "second", 600),
        event(
            3,
            "second",
            700,
            DayCheckpoint {
                applied_secs: 100,
                load_mix: togi_journal::LoadMix::default(),
            },
        ),
        end(4, "first", -5),
        end(5, "second", -10),
    ];
    for (index, name) in recorded.iter().enumerate() {
        events.push(event(
            10 + index as u64,
            "next",
            2000,
            DayRecord {
                boot: togi_journal::CrashedBoot::UncleanEnd { boot: boot(name) },
                offsets: togi_journal::RecordedOffsets::new(two_cores(-1)).unwrap(),
                source: togi_journal::OffsetSource::ReadBack { at: at(1) },
                running: Running::Idle,
                note: None,
            },
        ));
    }
    DayCrashes::of(&events)
}

/// Answers from a script, and keeps what it was asked and told.
struct Script {
    lines: VecDeque<String>,
    asked: Vec<String>,
    said: Vec<String>,
}

impl Script {
    fn new(lines: &[&str]) -> Self {
        Self {
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            asked: Vec::new(),
            said: Vec::new(),
        }
    }
}

impl Ask for Script {
    fn say(&mut self, text: &str) {
        self.said.push(text.to_owned());
    }

    fn ask(&mut self, question: &str) -> io::Result<Option<String>> {
        self.asked.push(question.to_owned());
        Ok(self.lines.pop_front())
    }
}

fn given(args: &[&str]) -> Result<Given, Error> {
    use clap::Parser as _;
    #[derive(clap::Parser)]
    struct Wrapper {
        #[command(flatten)]
        args: Args,
    }
    let wrapper =
        Wrapper::try_parse_from(std::iter::once("record").chain(args.iter().copied())).unwrap();
    Given::of(&wrapper.args, &TimeZone::UTC)
}

fn by_flags(args: &[&str], crashes: &DayCrashes) -> Result<Draft, Error> {
    collect(&given(args)?, crashes, false, &mut Quiet, &TimeZone::UTC)
}

fn by_prompts(
    args: &[&str],
    lines: &[&str],
    crashes: &DayCrashes,
) -> (Result<Draft, Error>, Script) {
    let mut script = Script::new(lines);
    let draft =
        given(args).and_then(|given| collect(&given, crashes, true, &mut script, &TimeZone::UTC));
    (draft, script)
}

const ENTERED: &str = "2026-10-19T12:00:00Z";

#[test]
fn flags_and_prompts_describe_a_read_back_boot_identically() {
    let crashes = crashes(&[]);
    let flags = by_flags(
        &[
            "--boot",
            "second",
            "--running",
            "build",
            "--note",
            "a Nix build",
        ],
        &crashes,
    )
    .unwrap();
    // 2 picks the second boot, the empty answer keeps its read-back offsets.
    let (prompts, _) = by_prompts(&[], &["2", "", "build", "a Nix build"], &crashes);
    assert_eq!(prompts.unwrap(), flags);
    assert_eq!(
        flags.crash,
        Crash::Observed {
            boot: boot("second"),
            entered: None
        }
    );
}

#[test]
fn flags_and_prompts_describe_entered_offsets_for_a_boot_identically() {
    let crashes = crashes(&[]);
    let flags = by_flags(
        &[
            "--boot",
            "first",
            "--offset",
            "ccd0=-25",
            "--offset",
            "core1=-30",
            "--entered-at",
            ENTERED,
            "--running",
            "desktop",
        ],
        &crashes,
    )
    .unwrap();
    let (prompts, _) = by_prompts(
        &[],
        &["first", "ccd0=-25 core1=-30", ENTERED, "desktop", ""],
        &crashes,
    );
    assert_eq!(prompts.unwrap(), flags);
    assert!(matches!(
        flags.crash,
        Crash::Observed { entered: Some(Entered { specs, .. }), .. } if specs.len() == 2
    ));
}

#[test]
fn flags_and_prompts_describe_an_unobserved_crash_identically() {
    let crashes = crashes(&[]);
    let flags = by_flags(
        &[
            "--unobserved-at",
            "2026-10-19T11:00:00Z",
            "--offset",
            "all=-20",
            "--entered-at",
            ENTERED,
            "--running",
            "other",
            "--note",
            "freeze",
        ],
        &crashes,
    )
    .unwrap();
    let (prompts, _) = by_prompts(
        &[],
        &[
            "none",
            "2026-10-19T11:00:00Z",
            "all=-20",
            ENTERED,
            "other",
            "freeze",
        ],
        &crashes,
    );
    assert_eq!(prompts.unwrap(), flags);
}

#[test]
fn a_flag_replaces_its_question_and_leaves_the_others() {
    let crashes = crashes(&[]);
    let (draft, script) = by_prompts(
        &["--boot", "second", "--running", "game"],
        &["", ""],
        &crashes,
    );
    assert_eq!(draft.unwrap().running, Running::Game);
    assert_eq!(script.asked.len(), 2, "{:?}", script.asked);
    assert!(script.asked[0].starts_with("Offsets the observer read back"));
    assert!(script.asked[1].starts_with("A note"));
}

#[test]
fn the_default_boot_is_the_newest_without_a_record() {
    let (draft, script) = by_prompts(&[], &["", "", "", ""], &crashes(&["second"]));
    assert!(matches!(
        draft.unwrap().crash,
        Crash::Observed { boot: chosen, .. } if chosen == boot("first")
    ));
    assert!(script.asked[0].ends_with("[1]:"), "{}", script.asked[0]);
    assert!(
        script
            .said
            .iter()
            .any(|line| line.contains("already recorded"))
    );
}

#[test]
fn with_every_boot_recorded_the_default_is_a_crash_no_observer_saw() {
    let (draft, script) = by_prompts(
        &[],
        &["", "2026-10-19 11:00", "all=-20", ENTERED, "", ""],
        &crashes(&["first", "second"]),
    );
    assert!(matches!(
        draft.unwrap().crash,
        Crash::Unobserved { at: crash, .. } if crash == at(1_792_407_600)
    ));
    assert!(script.asked[0].ends_with("[none]:"), "{}", script.asked[0]);
}

#[test]
fn without_a_recorded_end_the_prompt_says_so_and_defaults_to_none() {
    let (draft, script) = by_prompts(
        &[],
        &["", "2026-10-19T11:00:00Z", "all=-20", ENTERED, "", ""],
        &DayCrashes::default(),
    );
    assert!(matches!(draft.unwrap().crash, Crash::Unobserved { .. }));
    assert_eq!(
        script.said[0],
        "The journal holds no unclean end of a day observer's boot."
    );
}

#[test]
fn an_answer_that_does_not_parse_is_asked_again() {
    let (draft, script) = by_prompts(&[], &["3", "second", "", "loud", "idle", ""], &crashes(&[]));
    assert!(draft.is_ok());
    assert!(
        script
            .said
            .iter()
            .any(|line| line.contains("not one of the numbers"))
    );
    assert!(
        script
            .said
            .iter()
            .any(|line| line.contains("expected one of"))
    );
}

#[test]
fn a_malformed_replacement_offset_is_explained_and_asked_again() {
    let crashes = crashes(&[]);
    let (draft, script) = by_prompts(
        &["--boot", "second", "--running", "build"],
        &["core0=oops", "ccd0=-25 ccd1=-30", ENTERED, ""],
        &crashes,
    );
    let flags = by_flags(
        &[
            "--boot",
            "second",
            "--offset",
            "ccd0=-25",
            "--offset",
            "ccd1=-30",
            "--entered-at",
            ENTERED,
            "--running",
            "build",
        ],
        &crashes,
    )
    .unwrap();
    assert_eq!(draft.unwrap(), flags);
    assert_eq!(
        script.said,
        ["\"core0=oops\": \"oops\" is not a number"],
        "{:?}",
        script.said
    );
    assert!(script.asked[0].starts_with("Offsets the observer read back"));
    assert_eq!(script.asked[1], SPECS_QUESTION);
}

#[test]
fn the_input_ending_midway_is_an_error() {
    let (draft, _) = by_prompts(&[], &["second"], &crashes(&[]));
    assert!(matches!(draft, Err(Error::InputEnded)));
}

#[test_case(&[], "--boot or --unobserved-at"; "no crash")]
#[test_case(&["--unobserved-at", "2026-10-19T11:00:00Z"], "--offset"; "no offsets for an unobserved crash")]
#[test_case(&["--unobserved-at", "2026-10-19T11:00:00Z", "--offset", "all=-20"], "--entered-at"; "offsets without when they were entered")]
fn without_a_terminal_a_missing_part_is_named(args: &[&str], missing: &str) {
    let error = by_flags(args, &crashes(&[])).unwrap_err();
    assert!(
        matches!(error, Error::Missing(name) if name == missing),
        "{error}"
    );
    assert_eq!(error.exit(), Exit::Usage);
}

#[test]
fn without_a_terminal_the_rest_takes_its_defaults() {
    let draft = by_flags(&["--boot", "second"], &crashes(&[])).unwrap();
    assert_eq!(draft.running, Running::Unknown);
    assert_eq!(draft.note, None);
}

#[test]
fn a_time_of_entry_without_offsets_is_refused() {
    let error = given(&["--boot", "first", "--entered-at", ENTERED])
        .err()
        .unwrap();
    assert!(matches!(error, Error::EnteredWithoutOffsets));
}

#[test_case("all=-20", OffsetSpec::All(-20); "all")]
#[test_case("ccd1=-25", OffsetSpec::Ccd(CcdId::new(1).unwrap(), -25); "a ccd")]
#[test_case("core3=-10", OffsetSpec::Core(CoreId::new(3).unwrap(), -10); "a core")]
fn an_offset_spec_names_what_it_covers(text: &str, spec: OffsetSpec) {
    assert_eq!(parse_spec(text).unwrap(), spec);
}

#[test_case("-20"; "no key")]
#[test_case("all=x"; "no number")]
#[test_case("ccd=-20"; "no ccd index")]
#[test_case("socket1=-20"; "unknown key")]
#[test_case("core99=-1"; "a core out of range")]
fn a_malformed_offset_spec_is_refused(text: &str) {
    assert!(parse_spec(text).is_err());
}

#[test]
fn the_prompt_takes_several_specs_on_one_line() {
    assert_eq!(
        parse_specs("all=0, ccd1=-25 core3=-10").unwrap(),
        vec![
            OffsetSpec::All(0),
            OffsetSpec::Ccd(CcdId::new(1).unwrap(), -25),
            OffsetSpec::Core(CoreId::new(3).unwrap(), -10)
        ]
    );
    assert!(parse_specs("  ").is_err());
}

#[test]
fn a_time_is_rfc_3339_or_local() {
    let zone = TimeZone::fixed(jiff::tz::offset(2));
    assert_eq!(
        parse_time("2026-10-19T11:00:00Z", &zone).unwrap(),
        at(1_792_407_600)
    );
    assert_eq!(
        parse_time("2026-10-19 13:00", &zone).unwrap(),
        at(1_792_407_600)
    );
    assert!(parse_time("yesterday", &zone).is_err());
}
