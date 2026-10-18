use std::time::Duration;

use togi_journal::Outcome::{Failure, Inconclusive, Pass};
use togi_journal::{
    Activity, CorePhase, CoreTransition, Decision, EventKind, HuntOutcome, Payload, SessionContext,
    TunerDecision,
};
use togi_machine::{CoreId, Regime, Signal};

use super::kit::*;
use crate::words::{
    Brief, History, Phrase, Tag, Tagging, Tone, age, candidate_word, carried_groups, core_ids,
    core_list, cores_named, cores_text, decision_line, decision_phrase, failure_line, group_word,
    hunt_end_phrase, kind_words, lateness, load_words, noun, on_cores, phase_line, phase_word,
    plural, regime_words, signal_words, solo_limit, tag_word, trial_end_tag, words,
    workload_display,
};
use crate::words::{decision_brief, decision_detail};
use togi_journal::{
    CheckingChain, CheckingStep, DeadEnd, DeadEndCondition, DeepeningRound, HuntSkipped, RoundEvent,
};
use togi_machine::CcdId;
use togi_tuner::{DecisionPayload, GroupState};

/// The kinds with no words of their own: not in the history, in the log under their kind name, in no story.
const SILENT: &[EventKind] = &[
    EventKind::SessionContext,
    EventKind::SessionBaseline,
    EventKind::SessionNotice,
    EventKind::SessionArchived,
    EventKind::SmuError,
    EventKind::ProfileApplied,
    EventKind::ProfileChange,
    EventKind::ProfileRestored,
    EventKind::TrialStart,
    EventKind::TrialProgress,
    EventKind::TrialSignal,
    EventKind::TrialSample,
    EventKind::TrialCarried,
    EventKind::FailureCarried,
    EventKind::HostRanking,
    EventKind::TunerWarning,
    EventKind::DefectAnswered,
    EventKind::BootSavedEntry,
    EventKind::BootLeaveReason,
    EventKind::JournalTorn,
    EventKind::StateRebuilt,
];

/// The kinds the log leaves out: the SMU dance and the preflight checks.
const UNLISTED: &[EventKind] = &[
    EventKind::PreflightCheck,
    EventKind::SmuIntent,
    EventKind::SmuWrite,
    EventKind::SmuReadback,
];

fn quiet(kind: EventKind) -> bool {
    let table = words(kind);
    matches!(table.history, History::Silent)
        && matches!(table.tag, Tagging::Kind)
        && matches!(table.compact, Brief::Silent)
        && matches!(table.detailed, Phrase::Silent)
}

#[test]
fn every_kind_has_its_words_or_says_it_is_silent() {
    assert_eq!(EventKind::ALL.len(), 50);
    for &kind in EventKind::ALL {
        let table = words(kind);
        assert_eq!(
            quiet(kind),
            SILENT.contains(&kind) || UNLISTED.contains(&kind),
            "{kind}: a kind with no entry must be listed as silent, and a silent one must have none"
        );
        assert_eq!(
            table.listed,
            !UNLISTED.contains(&kind),
            "{kind}: only the SMU and preflight kinds are left out of the log"
        );
    }
    for kind in SILENT.iter().chain(UNLISTED) {
        assert!(quiet(*kind), "{kind}");
    }
}

#[test]
fn the_tag_of_a_silent_kind_is_its_name() {
    let context: Payload = SessionContext {
        bios_version: String::new(),
        board: String::new(),
        cpu_model: String::new(),
        microcode: String::new(),
        boost_limit_mhz: 0,
    }
    .into();
    assert_eq!(tag_word(&context), "session context");
    let reset = Payload::CommandReset(togi_journal::CommandReset {
        target: togi_journal::ResetTarget::All,
    });
    assert_eq!(tag_word(&reset), "command reset");
    let cycle: Payload = cycle(1, togi_journal::CycleEvent::Start).into();
    assert_eq!(tag_word(&cycle), "cycle");
}

#[test]
fn the_history_line_of_a_kind_is_its_tag() {
    let mut h = Hist::three();
    h.push(crash("boot", None).into());
    let (entries, _) = h.facts.history.recent();
    assert_eq!(entries[0].tag, Tag::Crash);
    assert_eq!(tag_word(&crash("boot", None).into()), "crash");
}

pub(super) fn decision(kind: Decision, from: i64, to: i64) -> TunerDecision {
    TunerDecision {
        core: core(3),
        activity: Activity::Checking,
        decision: kind,
        from_offset: off(from),
        to_offset: off(to),
        pass: None,
        failure_point: Some(off(from)),
        workloads: Vec::new(),
        reason: String::new(),
    }
}

#[test]
fn each_decision_has_its_tag_tone_and_phrases() {
    let rows = [
        (
            Decision::StepDeeper,
            Tag::Step,
            Tone::Plain,
            "core 03 passed -10; searches -15 on its next turn",
            "core 03 searches -15 on its next turn",
        ),
        (
            Decision::CheckSoloLimit,
            Tag::Confirm,
            Tone::Plain,
            "core 03 confirming -15 as its solo limit",
            "core 03 starts confirming -15 as its solo limit",
        ),
        (
            Decision::Deepen,
            Tag::Room,
            Tone::Good,
            "core 03 -10 → -15",
            "core 03 deepens -10 → -15",
        ),
        (
            Decision::Yield,
            Tag::Room,
            Tone::Plain,
            "core 03 yields -10 → -15, back to its round baseline",
            "core 03 yields -10 → -15, back to its round baseline",
        ),
        (
            Decision::Hold,
            Tag::Hold,
            Tone::Warn,
            "core 03 failure held; tuning stays at -10",
            "core 03 failure held at -10; no tuning move",
        ),
        (
            Decision::Backoff,
            Tag::Backoff,
            Tone::Warn,
            "core 03 -10 → -15",
            "core 03 -10 → -15",
        ),
    ];
    for (kind, tag, tone, line, phrase) in rows {
        let d = decision(kind, -10, -15);
        let said = decision_line(&d);
        assert_eq!(
            (said.tag, said.tone, said.text.as_str()),
            (tag, tone, line),
            "{kind}"
        );
        assert_eq!(decision_phrase(&d, None), phrase, "{kind}");
        let payload = Payload::from(d);
        assert_decision_words(&payload);
        let table = words(EventKind::TunerDecision);
        let Tagging::By(of) = table.tag else {
            panic!("a decision is tagged by its payload");
        };
        assert_eq!(of(&payload), tag);
        let Phrase::Of(of) = table.detailed else {
            panic!("a decision has a phrase");
        };
        assert_eq!(of(&payload).as_deref(), Some(phrase));
        let Brief::Of(of) = table.compact else {
            panic!("a decision may have a compact outcome");
        };
        assert_eq!(
            of(&payload).as_deref(),
            (kind == Decision::Backoff).then_some("backs off"),
            "{kind}"
        );
    }
    let named = decision(Decision::Backoff, -30, -16);
    assert_eq!(
        decision_phrase(&named, Some(core(3))),
        "core 03 fails at -30, -30 → -16",
        "a backoff of the core a failure named records its failure point"
    );
    assert_eq!(decision_phrase(&named, Some(core(4))), "core 03 -30 → -16");
}

fn phase(from: Option<CorePhase>, to: CorePhase, offset: i64, pass: Option<i64>) -> CoreTransition {
    let mut start = Start::search(offset);
    (start.from, start.to, start.pass) = (from, to, pass);
    transition(1, start)
}

#[test]
fn a_solo_limit_is_the_offset_the_core_passed_alone() {
    let exit = phase(Some(CorePhase::Search), CorePhase::HasRoom, -19, Some(-20));
    assert_eq!(solo_limit(&exit), Some(-20));
    assert_eq!(phase_line(&exit).unwrap().text, "core 01 solo limit -20");
    let bare = phase(Some(CorePhase::Search), CorePhase::HasRoom, -9, None);
    assert_eq!(
        solo_limit(&bare),
        None,
        "an exit with no pass has no solo limit"
    );
    assert!(phase_line(&bare).is_none());
    let again = phase(Some(CorePhase::AtLimit), CorePhase::Search, 0, None);
    assert_eq!(
        phase_line(&again).unwrap().text,
        "core 01 starts over from 0"
    );
    let room = phase(Some(CorePhase::AtLimit), CorePhase::HasRoom, -12, None);
    assert_eq!(
        phase_line(&room).map(|line| (line.tag, line.text)),
        Some((Tag::Room, "core 01 has room again at -12".to_owned()))
    );
    for (from, to) in [
        (None, CorePhase::Search),
        (None, CorePhase::HasRoom),
        (None, CorePhase::AtLimit),
        (Some(CorePhase::HasRoom), CorePhase::HasRoom),
        (Some(CorePhase::HasRoom), CorePhase::AtLimit),
        (Some(CorePhase::AtLimit), CorePhase::AtLimit),
    ] {
        assert!(
            phase_line(&phase(from, to, -12, None)).is_none(),
            "{from:?} → {to:?} shows on the core's row"
        );
    }
}

#[test]
fn trial_ends_are_tagged_by_how_they_ended() {
    let end =
        |outcome, signal| trial_end_tag(&end_payload(&"t".to_owned().into(), outcome, signal));
    assert_eq!(end(Pass, None), Tag::Pass);
    assert_eq!(end(Failure, Some(Signal::Crash)), Tag::Crash);
    assert_eq!(end(Failure, Some(Signal::Stall)), Tag::Fail);
    assert_eq!(end(Inconclusive, None), Tag::Unclear);
    assert_eq!(end(Inconclusive, Some(Signal::Stall)), Tag::Unclear);
    assert_eq!(end(Inconclusive, Some(Signal::Crash)), Tag::Crash);
}

#[test]
fn hunt_ends_in_the_detailed_story() {
    let end = |result, named| hunt_end_phrase(&hunt_end(2, result, &[0, 1]), named);
    assert_eq!(
        end(HuntOutcome::Direct, true),
        ("the hunt ends".to_owned(), false)
    );
    assert_eq!(
        end(HuntOutcome::Culprit, false).0,
        "hunt 2 ends: 00 01 named"
    );
    assert_eq!(
        end(HuntOutcome::Fallback, false),
        ("hunt 2 unresolved".to_owned(), true)
    );
    assert_eq!(
        end(HuntOutcome::Combination, false).0,
        "hunt 2 keeps 00 01 together"
    );
    assert_eq!(end(HuntOutcome::Cancelled, false).0, "hunt 2 cancelled");
    assert_eq!(
        end(HuntOutcome::Loaded, false).0,
        "hunt 2 keeps the failure on loaded 00 01"
    );
}

#[test]
fn the_regime_and_signal_words() {
    for (regime, sentence, kind) in [
        (Regime::R1, "light load", "light"),
        (Regime::R2, "heavy vector load", "heavy vector"),
        (Regime::R3, "load steps", "load steps"),
        (Regime::R4, "partial load", "partial load"),
        (Regime::R5, "both threads", "both threads"),
        (Regime::R6, "idle with short bursts", "idle + bursts"),
        (Regime::R7, "all-core load", "all-core"),
    ] {
        assert_eq!(regime_words(regime), sentence);
        assert_eq!(kind_words(regime), kind);
    }
    for regime in togi_machine::REGIMES {
        let expected = if regime == Regime::R6 {
            "idle + bursts"
        } else {
            regime_words(regime)
        };
        assert_eq!(load_words(regime), expected);
    }
    for (signal, words) in [
        (Signal::ComputationError, "wrong result"),
        (Signal::UnexpectedExit, "the backend quit"),
        (Signal::Stall, "the backend stalled"),
        (Signal::CorrectedMce, "corrected hardware error"),
        (Signal::UncorrectedMce, "uncorrected hardware error"),
        (Signal::Crash, "crash"),
    ] {
        assert_eq!(signal_words(signal), words);
    }
}

#[test]
fn the_phase_word_is_shared_with_status() {
    assert_eq!(phase_word(CorePhase::Search), "SEARCH");
    assert_eq!(phase_word(CorePhase::HasRoom), "HAS ROOM");
    assert_eq!(phase_word(CorePhase::AtLimit), "AT LIMIT");
    assert_eq!(phase_word(Activity::Checking), "checking");
    assert_eq!(phase_word(Activity::Deepening), "deepening");
}

#[test]
fn the_group_and_candidate_words_are_shared_with_status() {
    assert_eq!(group_word(GroupState::Running), "running");
    assert_eq!(group_word(GroupState::Passed), "passed");
    assert_eq!(group_word(GroupState::Failed), "failed");
    assert_eq!(group_word(GroupState::Skipped), "skipped");
    assert_eq!(candidate_word(true), "moving");
    assert_eq!(candidate_word(false), "checked again in place");
}

#[test]
fn counting_words() {
    assert_eq!(
        (noun(1, "step"), noun(2, "step"), noun(2, "crash")),
        ("step".into(), "steps".into(), "crashes".into())
    );
    assert_eq!(noun(2, "wish"), "wishes");
    assert_eq!(plural(0, "group"), "0 groups");
    assert_eq!(plural(1, "solo limit"), "1 solo limit");
}

#[test]
fn cores_are_named_compactly() {
    let ids = |ids: &[u8]| ids.iter().map(|&id| core(id)).collect::<Vec<CoreId>>();
    assert_eq!(core_list(ids(&[])), "no cores");
    assert_eq!(core_list(ids(&[3])), "core 03");
    assert_eq!(core_list(ids(&[0, 1])), "cores 00, 01");
    assert_eq!(
        core_list(ids(&[0, 1, 2, 5, 7, 8, 9, 10])),
        "cores 00-02, 05, 07-10"
    );
    assert_eq!(core_ids(ids(&[0, 1, 2, 5])), "00-02 05");
    assert_eq!(core_ids(ids(&[3])), "03");
    assert_eq!(cores_text(set(&[3]), 8), "core 03");
    assert_eq!(cores_text(set(&[0, 1, 2, 3]), 4), "all 4 cores");
    assert_eq!(cores_text(set(&[0, 2]), 4), "cores 00, 02");
    assert_eq!(on_cores(set(&[3])), "on core 03");
    assert_eq!(on_cores(set(&[0, 1, 2])), "on 00-02");
    assert_eq!(cores_named(set(&[3])), "core 03");
    assert_eq!(cores_named(set(&[0, 1, 2, 5])), "cores 00-02 05");
}

#[test]
fn spans_and_lateness() {
    let secs = Duration::from_secs;
    assert_eq!(
        (age(secs(42)), age(secs(185)), age(secs(7800))),
        ("42s".into(), "3m05s".into(), "2h10m".into())
    );
    let planned = Some(secs(90));
    assert_eq!(lateness(Some(secs(10)), planned), "early in");
    assert_eq!(lateness(Some(secs(45)), planned), "midway through");
    assert_eq!(lateness(Some(secs(80)), planned), "late in");
    assert_eq!(lateness(None, planned), "during");
    assert_eq!(lateness(Some(secs(10)), Some(Duration::ZERO)), "during");
}

#[test]
fn a_variant_workload_shows_its_base() {
    for regime in togi_machine::REGIMES {
        for entry in togi_machine::workloads(regime) {
            let shown = workload_display(entry.id());
            assert!(!shown.is_empty());
            if !entry.base.is_empty() {
                assert_eq!(
                    shown,
                    togi_machine::workload_by_id(entry.base).unwrap().label
                );
            }
        }
    }
}

fn phrase(payload: &Payload) -> Option<String> {
    let Phrase::Of(of) = words(payload.kind()).detailed else {
        panic!("{} has no phrase of its own", payload.kind());
    };
    let said = of(payload);
    assert_decision_words(payload);
    said
}

fn compact(payload: &Payload) -> Option<String> {
    let Brief::Of(of) = words(payload.kind()).compact else {
        panic!("{} has no compact words", payload.kind());
    };
    let said = of(payload);
    assert_decision_words(payload);
    said
}

/// The words of a forecast decision are the table's words for the event it would be.
fn assert_decision_words(payload: &Payload) {
    let decision = match payload.clone() {
        Payload::Failure(d) => DecisionPayload::from(d),
        Payload::TunerWarning(d) => d.into(),
        Payload::DeadEnd(d) => d.into(),
        Payload::CorePhase(d) => d.into(),
        Payload::TunerDecision(d) => d.into(),
        Payload::ProfileChange(d) => d.into(),
        Payload::CheckingCycle(d) => d.into(),
        Payload::CheckingStep(d) => d.into(),
        Payload::CheckingChain(d) => d.into(),
        Payload::HuntStart(d) => d.into(),
        Payload::HuntGroup(d) => d.into(),
        Payload::HuntEnd(d) => d.into(),
        Payload::HuntSkipped(d) => d.into(),
        Payload::Combination(d) => d.into(),
        Payload::DeepeningRound(d) => d.into(),
        Payload::SessionStart(_)
        | Payload::SessionContext(_)
        | Payload::SessionBaseline(_)
        | Payload::SessionNotice(_)
        | Payload::SessionWarning(_)
        | Payload::SessionArchived(_)
        | Payload::SessionCarried(_)
        | Payload::ConfigLoaded(_)
        | Payload::PreflightCheck(_)
        | Payload::SmuIntent(_)
        | Payload::SmuWrite(_)
        | Payload::SmuReadback(_)
        | Payload::SmuError(_)
        | Payload::ProfileApplied(_)
        | Payload::ProfileRestored(_)
        | Payload::TrialIntent(_)
        | Payload::TrialStart(_)
        | Payload::TrialProgress(_)
        | Payload::TrialSignal(_)
        | Payload::TrialSample(_)
        | Payload::TrialEnd(_)
        | Payload::TrialCarried(_)
        | Payload::FailureCarried(_)
        | Payload::Mce(_)
        | Payload::CrashDetected(_)
        | Payload::HostRanking(_)
        | Payload::BackendRetry(_)
        | Payload::CommandReset(_)
        | Payload::DefectFound(_)
        | Payload::DefectAnswered(_)
        | Payload::BootSavedEntry(_)
        | Payload::BootLeaveReason(_)
        | Payload::Shutdown(_)
        | Payload::JournalTorn(_)
        | Payload::StateRebuilt(_) => return,
    };
    let table = words(payload.kind());
    let detailed = match table.detailed {
        Phrase::Of(of) => of(payload),
        Phrase::Silent | Phrase::Contextual => None,
    };
    let brief = match table.compact {
        Brief::Of(of) => of(payload),
        Brief::Silent => None,
    };
    assert_eq!(decision_detail(&decision), detailed, "{}", payload.kind());
    assert_eq!(decision_brief(&decision), brief, "{}", payload.kind());
}

fn history_line(payload: &Payload) -> Option<crate::words::Line> {
    let History::Line(of) = words(payload.kind()).history else {
        panic!("{} has no line of its own", payload.kind());
    };
    of(payload)
}

#[test]
fn chains_cycles_and_rounds_have_phrases() {
    let chain = |cores: &[u8]| -> Payload {
        CheckingChain {
            cycle: nz(1),
            step: nz(1),
            ccd: CcdId::new(1).unwrap(),
            workload: togi_machine::workloads(Regime::R7)[0].id(),
            groups: Vec::new(),
            cores: set(cores),
            source_seqs: Vec::new(),
            profile: offsets(&[0]),
            part: nz(1),
            rederived: None,
        }
        .into()
    };
    assert_eq!(phrase(&chain(&[])).as_deref(), Some("CCD 1 partials end"));
    assert_eq!(
        phrase(&chain(&[2, 3])).as_deref(),
        Some("CCD 1 partial on 02 03")
    );

    let ended = |passed, full| -> Payload {
        let mut end = cycle(2, togi_journal::CycleEvent::End);
        (end.passed, end.full) = (passed, full);
        end.missing = vec!["R2".to_owned(), "R5".to_owned()];
        end.reason = Some("a failure".to_owned());
        end.into()
    };
    let start: Payload = cycle(2, togi_journal::CycleEvent::Start).into();
    assert_eq!(phrase(&start).as_deref(), Some("cycle 2 starts"));
    assert_eq!(
        phrase(&ended(true, true)).as_deref(),
        Some("cycle 2 passed, a full cycle")
    );
    assert_eq!(
        phrase(&ended(true, false)).as_deref(),
        Some("cycle 2 passed")
    );
    assert_eq!(
        phrase(&ended(false, false)).as_deref(),
        Some("cycle 2 ends")
    );
    let line = |payload: &Payload| history_line(payload).unwrap().text;
    assert_eq!(line(&start), "cycle 2 started · 0 steps");
    assert_eq!(line(&ended(true, true)), "cycle 2 passed, a full cycle");
    assert_eq!(
        line(&ended(true, false)),
        "cycle 2 passed but missing R2, R5"
    );
    assert_eq!(line(&ended(false, false)), "cycle 2 ended early: a failure");

    let round = |event, passed| -> Payload {
        DeepeningRound {
            round: nz(4),
            event,
            base: offsets(&[]),
            base_seq: None,
            target: offsets(&[]),
            profile: offsets(&[]),
            cores: set(&[]),
            ranking: Vec::new(),
            trials: None,
            trial_s: None,
            passed,
            reason: None,
        }
        .into()
    };
    assert_eq!(
        phrase(&round(RoundEvent::Start, false)).as_deref(),
        Some("deepening round 4 starts")
    );
    assert_eq!(
        phrase(&round(RoundEvent::End, true)).as_deref(),
        Some("deepening round 4 passed")
    );
    assert_eq!(
        phrase(&round(RoundEvent::End, false)).as_deref(),
        Some("deepening round 4 ends")
    );
}

#[test]
fn steps_hunts_and_stops_have_phrases() {
    let step = |n| -> Payload {
        CheckingStep {
            cycle: nz(1),
            step: nz(n),
            profile: offsets(&[]),
        }
        .into()
    };
    assert_eq!(phrase(&step(1)), None);
    assert_eq!(phrase(&step(3)).as_deref(), Some("step 2 is done"));

    let start: Payload = hunt_start(2, &[0, 2]).into();
    assert_eq!(phrase(&start).as_deref(), Some("hunt 2 starts over 00 02"));
    assert_eq!(compact(&start).as_deref(), Some("hunt 2"));
    let end: Payload = hunt_end(2, HuntOutcome::Fallback, &[0]).into();
    assert_eq!(compact(&end).as_deref(), Some("hunt ends"));

    let skipped: Payload = HuntSkipped {
        failure: seq(1),
        reason: String::new(),
    }
    .into();
    assert_eq!(phrase(&skipped).as_deref(), Some("no hunt"));
    assert_eq!(compact(&skipped).as_deref(), Some("no hunt"));
    assert_eq!(history_line(&skipped).unwrap().text, "not hunted");
    let held: Payload = HuntSkipped {
        failure: seq(1),
        reason: "held: first unattributed failure".into(),
    }
    .into();
    assert_eq!(
        phrase(&held).as_deref(),
        Some("no hunt: held: first unattributed failure")
    );
    assert_eq!(compact(&held).as_deref(), Some("no hunt"));
    assert_eq!(
        history_line(&held).unwrap().text,
        "not hunted: held: first unattributed failure"
    );

    let combo: Payload = combination(3, 1, &[(0, -30), (2, -28)], false).into();
    assert_eq!(compact(&combo).as_deref(), Some("C3"));
    assert_eq!(phrase(&combo).as_deref(), Some("C3 over 00 02"));
    assert_eq!(history_line(&combo).unwrap().text, "C3: 00 -30  02 -28");
    let fallback: Payload = combination(4, 2, &[(0, -30), (2, -28)], true).into();
    assert_eq!(
        history_line(&fallback).unwrap().text,
        "C4 over 00 02 · hunt 2 unresolved"
    );

    let stop: Payload = DeadEnd {
        condition: DeadEndCondition::FailureAtZero,
        core: None,
        detail: "x".to_owned(),
        action: None,
    }
    .into();
    assert_eq!(compact(&stop).as_deref(), Some("tuning stops"));
    assert_eq!(
        phrase(&stop).as_deref(),
        Some("tuning stops: failure at zero")
    );
}

#[test]
fn core_phases_have_phrases() {
    let phase = |from, to, pass: Option<i64>| -> Payload {
        let mut start = Start::search(-12);
        (start.from, start.to, start.pass) = (from, to, pass);
        transition(1, start).into()
    };
    let search = Some(CorePhase::Search);
    assert_eq!(
        phrase(&phase(search, CorePhase::HasRoom, Some(-13))).as_deref(),
        Some("core 01 solo limit -13")
    );
    assert_eq!(
        phrase(&phase(search, CorePhase::AtLimit, None)),
        None,
        "an exit with no pass names no solo limit"
    );
    assert_eq!(
        phrase(&phase(Some(CorePhase::AtLimit), CorePhase::Search, None)).as_deref(),
        Some("core 01 starts its search over")
    );
    assert_eq!(
        phrase(&phase(None, CorePhase::Search, None)).as_deref(),
        Some("core 01 starts its search"),
        "the first start is not a start over"
    );
    assert_eq!(
        phrase(&phase(Some(CorePhase::AtLimit), CorePhase::HasRoom, None)),
        None
    );
    assert_eq!(phrase(&phase(None, CorePhase::HasRoom, None)), None);
    let Tagging::By(tag) = words(EventKind::CorePhase).tag else {
        panic!("a core phase is tagged by its payload");
    };
    for (from, to, pass, word) in [
        (search, CorePhase::HasRoom, Some(-13), Tag::Limit),
        (search, CorePhase::HasRoom, None, Tag::Room),
        (search, CorePhase::AtLimit, None, Tag::Limit),
        (None, CorePhase::Search, None, Tag::Step),
        (None, CorePhase::HasRoom, None, Tag::Room),
        (None, CorePhase::AtLimit, None, Tag::Limit),
        (
            Some(CorePhase::AtLimit),
            CorePhase::HasRoom,
            None,
            Tag::Room,
        ),
        (Some(CorePhase::HasRoom), CorePhase::Search, None, Tag::Step),
    ] {
        assert_eq!(tag(&phase(from, to, pass)), word, "{from:?} → {to:?}");
    }
}

#[test]
fn a_phrase_applies_only_to_its_own_kind() {
    let other: Payload = warning("x");
    for kind in [
        EventKind::TunerDecision,
        EventKind::CorePhase,
        EventKind::CheckingCycle,
        EventKind::CheckingStep,
        EventKind::CheckingChain,
        EventKind::HuntStart,
        EventKind::Combination,
        EventKind::DeepeningRound,
        EventKind::DeadEnd,
        EventKind::Mce,
    ] {
        let table = words(kind);
        if let Phrase::Of(of) = table.detailed {
            assert_eq!(of(&other), None, "{kind}");
        }
        if let (Brief::Of(of), false) = (table.compact, kind == EventKind::DeadEnd) {
            assert_eq!(of(&other), None, "{kind}");
        }
        if let (History::Line(of), false) = (table.history, kind == EventKind::CheckingChain) {
            assert_eq!(of(&other), None, "{kind}");
        }
    }
    for kind in [
        EventKind::TrialEnd,
        EventKind::Failure,
        EventKind::TunerDecision,
        EventKind::HuntGroup,
        EventKind::CorePhase,
    ] {
        let Tagging::By(of) = words(kind).tag else {
            panic!("{kind} is tagged by its payload");
        };
        let fallback = match kind {
            EventKind::TrialEnd => Tag::Unclear,
            EventKind::Failure => Tag::Fail,
            EventKind::TunerDecision => Tag::Backoff,
            EventKind::HuntGroup => Tag::Group,
            EventKind::SessionStart
            | EventKind::SessionContext
            | EventKind::SessionBaseline
            | EventKind::SessionNotice
            | EventKind::SessionWarning
            | EventKind::SessionArchived
            | EventKind::SessionCarried
            | EventKind::ConfigLoaded
            | EventKind::PreflightCheck
            | EventKind::SmuIntent
            | EventKind::SmuWrite
            | EventKind::SmuReadback
            | EventKind::SmuError
            | EventKind::ProfileApplied
            | EventKind::ProfileChange
            | EventKind::ProfileRestored
            | EventKind::TrialIntent
            | EventKind::TrialStart
            | EventKind::TrialProgress
            | EventKind::TrialSignal
            | EventKind::TrialSample
            | EventKind::TrialCarried
            | EventKind::FailureCarried
            | EventKind::Mce
            | EventKind::CrashDetected
            | EventKind::CorePhase
            | EventKind::CheckingCycle
            | EventKind::CheckingStep
            | EventKind::CheckingChain
            | EventKind::HostRanking
            | EventKind::HuntStart
            | EventKind::HuntEnd
            | EventKind::HuntSkipped
            | EventKind::Combination
            | EventKind::DeepeningRound
            | EventKind::TunerWarning
            | EventKind::BackendRetry
            | EventKind::CommandReset
            | EventKind::DefectFound
            | EventKind::DefectAnswered
            | EventKind::DeadEnd
            | EventKind::BootSavedEntry
            | EventKind::BootLeaveReason
            | EventKind::Shutdown
            | EventKind::JournalTorn
            | EventKind::StateRebuilt => Tag::Limit,
        };
        assert_eq!(of(&other), fallback, "{kind}");
    }
}

#[test]
fn a_hunt_group_is_tagged_by_how_it_was_answered() {
    let Tagging::By(tag) = words(EventKind::HuntGroup).tag else {
        panic!("a group is tagged by its payload");
    };
    let plain = hunt_group(1, 1, &[0]);
    assert_eq!(tag(&plain.clone().into()), Tag::Group);
    let mut probe = plain.clone();
    probe.probe = Some(togi_journal::Member {
        core: core(0),
        offset: off(-1),
    });
    assert_eq!(tag(&probe.into()), Tag::Probe);
    let mut inferred = plain.clone();
    inferred.inferred = Some(togi_journal::Inference::Pass);
    assert_eq!(tag(&inferred.into()), Tag::Skip);
    let mut skipped = plain;
    skipped.skipped = true;
    assert_eq!(tag(&skipped.into()), Tag::Skip);
    assert_eq!(
        carried_groups(2, 2, togi_journal::Inference::Pass),
        ("group 2 answered by existing evidence".to_owned(), true)
    );
    assert_eq!(
        carried_groups(2, 4, togi_journal::Inference::Failure),
        ("groups 2-4 failed on existing evidence".to_owned(), false)
    );
}

#[test]
fn every_tag_has_a_word() {
    for (tag, word) in [
        (Tag::Unclear, "unclear"),
        (Tag::Reset, "reset"),
        (Tag::Mce, "mce"),
        (Tag::Note, "note"),
        (Tag::Confirm, "confirm"),
        (Tag::Resume, "resume"),
        (Tag::Hold, "hold"),
    ] {
        assert_eq!(tag.word(), word);
    }
}

#[test]
fn an_idle_failure_is_worded_but_a_crash_is_not() {
    let idle = failure(Signal::Stall, None);
    assert_eq!(
        failure_line(&idle).unwrap().text,
        "idle with the offsets applied, the backend stalled"
    );
    assert!(failure_line(&failure(Signal::Crash, None)).is_none());
    assert!(failure_line(&failure(Signal::Stall, Some("t1"))).is_none());
    let mut unnamed = failure(Signal::Stall, Some("t1"));
    unnamed.attribution = togi_journal::Attribution::Attributed;
    assert!(failure_line(&unnamed).is_none(), "attributed to no core");
}

#[test]
fn the_log_tags_each_kind_by_its_word() {
    let mut known = failure(Signal::ComputationError, None);
    known.known_failure = Some(seq(1));
    let round: Payload = DeepeningRound {
        round: nz(1),
        event: RoundEvent::Start,
        base: offsets(&[]),
        base_seq: None,
        target: offsets(&[]),
        profile: offsets(&[]),
        cores: set(&[]),
        ranking: Vec::new(),
        trials: None,
        trial_s: None,
        passed: false,
        reason: None,
    }
    .into();
    let step: Payload = CheckingStep {
        cycle: nz(1),
        step: nz(1),
        profile: offsets(&[]),
    }
    .into();
    let dead_end: Payload = DeadEnd {
        condition: DeadEndCondition::Smu,
        core: None,
        detail: String::new(),
        action: None,
    }
    .into();
    let shutdown: Payload = togi_journal::Shutdown {
        reason: togi_journal::ShutdownReason::Signal,
        kernel_cursor: None,
        kernel_error: None,
        cycles: None,
    }
    .into();
    let trial = togi_journal::TrialId::from("t".to_owned());
    let mut probe = hunt_group(1, 2, &[0]);
    probe.probe = Some(togi_journal::Member {
        core: core(0),
        offset: off(-1),
    });
    let rows: Vec<(Payload, &str)> = vec![
        (known.into(), "skip"),
        (failure(Signal::Stall, None).into(), "fail"),
        (hunt_start(1, &[0]).into(), "hunt"),
        (hunt_end(1, HuntOutcome::Culprit, &[0]).into(), "hunt"),
        (
            HuntSkipped {
                failure: seq(1),
                reason: String::new(),
            }
            .into(),
            "hunt",
        ),
        (hunt_group(1, 1, &[0]).into(), "group"),
        (probe.into(), "probe"),
        (combination(1, 1, &[(0, -1)], false).into(), "combo"),
        (round, "round"),
        (step, "step"),
        (cycle(1, togi_journal::CycleEvent::Start).into(), "cycle"),
        (dead_end, "stop"),
        (shutdown, "stop"),
        (session_start(&[(0, 0)]), "start"),
        (crash("boot", None).into(), "crash"),
        (end_payload(&trial, Pass, None).into(), "pass"),
        (
            end_payload(&trial, Failure, Some(Signal::Stall)).into(),
            "fail",
        ),
        (
            end_payload(&trial, Failure, Some(Signal::Crash)).into(),
            "crash",
        ),
        (end_payload(&trial, Inconclusive, None).into(), "unclear"),
        (decision(Decision::Backoff, -10, -9).into(), "backoff"),
        (decision(Decision::StepDeeper, -10, -15).into(), "step"),
        (decision(Decision::Hold, -10, -10).into(), "hold"),
        (warning("x"), "session warning"),
        (smu_intent(0, -1), "smu intent"),
    ];
    for (payload, word) in rows {
        assert_eq!(tag_word(&payload), word, "{}", payload.kind());
    }
}
