use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use test_case::test_case;
use togi_journal::Outcome::{Failure, Inconclusive, Pass};
use togi_journal::{
    ApplicationId, ApplyCause, Binding, Budget, CcdCap, CcdEdge, CcdVector, Certainty,
    Classification, DayApplied, DayEvent, DayEventId, DayFreeze, DayHold, DayStep, DayVeto,
    DecisionId, Edge, EventKind, ExposureClass, Finite, FreezeChoice, FreezeSource, HoldChange,
    HoldReason, ImportCounts, NightId, NightPlan, NightQualification, NightReproduction,
    ObservedProfile, Payload, Placement, PlannedTask, QualificationPurpose, QualificationState,
    Queue, Rate, ReproductionState, ReproductionTarget, Resolution, SessionContext,
    SessionImported, Slope, StepCauses, StepKind, TaskId, TaskStatus, VetoState,
};
use togi_machine::{BootId, CcdId, CcdProfile, CoreId, Digest, PerCore, Regime, Signal};

use super::kit::*;
use crate::history::Entry;
use crate::words::{
    History, Tag, Tagging, Tone, age, core_ids, core_list, cores_named, cores_text, edge_words,
    failure_line, kind_words, lateness, load_words, noun, on_cores, plural, regime_words,
    signal_words, tag_word, trial_end_tag, words, workload_display,
};

/// The kinds with no history line: in the log under their kind name, and in no story.
const SILENT: &[EventKind] = &[
    EventKind::ExposureObserved,
    EventKind::DayInterval,
    EventKind::DayImported,
    EventKind::SessionContext,
    EventKind::SessionBaseline,
    EventKind::SessionNotice,
    EventKind::SessionArchived,
    EventKind::SmuError,
    EventKind::ProfileApplied,
    EventKind::ProfileChange,
    EventKind::ProfileRestored,
    EventKind::TrialIntent,
    EventKind::TaskStart,
    EventKind::TrialStart,
    EventKind::TrialProgress,
    EventKind::TrialSignal,
    EventKind::TrialSample,
    EventKind::TrialCarried,
    EventKind::FailureCarried,
    EventKind::SessionCarried,
    EventKind::HostRanking,
    EventKind::TunerWarning,
    EventKind::DefectAnswered,
    EventKind::BootSavedEntry,
    EventKind::BootLeaveReason,
    EventKind::JournalTorn,
    EventKind::StateRebuilt,
    EventKind::DayBootStart,
    EventKind::DayCheckpoint,
    EventKind::DayOffsetsChanged,
    EventKind::DayCleanStop,
    EventKind::DayUncleanEnd,
    EventKind::DayAnswer,
    EventKind::DayRecord,
    EventKind::DayFault,
];

/// The kinds the log leaves out: the SMU dance and the preflight checks.
const UNLISTED: &[EventKind] = &[
    EventKind::PreflightCheck,
    EventKind::SmuIntent,
    EventKind::SmuWrite,
    EventKind::SmuReadback,
];

/// The kinds whose history line needs the events before them.
const CONTEXTUAL: &[EventKind] = &[
    EventKind::SessionStart,
    EventKind::ConfigLoaded,
    EventKind::TrialEnd,
    EventKind::CrashDetected,
];

/// The kinds of earlier rulesets whose history line is the message the journal recorded.
const HISTORICAL: &[EventKind] = &[
    EventKind::TunerDecision,
    EventKind::CorePhase,
    EventKind::CheckingCycle,
    EventKind::CheckingStep,
    EventKind::CheckingChain,
    EventKind::HuntStart,
    EventKind::HuntGroup,
    EventKind::HuntEnd,
    EventKind::HuntSkipped,
    EventKind::Combination,
    EventKind::DeepeningRound,
];

/// The Ruleset 13 kinds with a history line of their own, with the tag their events carry.
const RULESET13: &[(EventKind, Tag)] = &[
    (EventKind::CcdEdge, Tag::Edge),
    (EventKind::CcdCap, Tag::Cap),
    (EventKind::NightPlan, Tag::Night),
    (EventKind::NightQualification, Tag::Qualify),
    (EventKind::NightReproduction, Tag::Repro),
    (EventKind::DayEvent, Tag::Day),
    (EventKind::DayVeto, Tag::Veto),
    (EventKind::DayStep, Tag::Step),
    (EventKind::DayApplied, Tag::Apply),
    (EventKind::DayHold, Tag::Hold),
    (EventKind::DayFreeze, Tag::Freeze),
    (EventKind::SessionImported, Tag::Import),
];

#[test]
fn every_kind_has_its_words_or_says_it_is_silent() {
    assert_eq!(EventKind::ALL.len(), 74);
    for &kind in EventKind::ALL {
        let table = words(kind);
        let silent = SILENT.contains(&kind) || UNLISTED.contains(&kind);
        assert_eq!(
            matches!(table.history, History::Silent) && matches!(table.tag, Tagging::Kind),
            silent,
            "{kind}: a kind with no entry must be listed as silent, and a silent one must have none"
        );
        assert_eq!(
            matches!(table.history, History::Contextual(_)),
            CONTEXTUAL.contains(&kind),
            "{kind}"
        );
        assert_eq!(
            table.listed,
            !UNLISTED.contains(&kind),
            "{kind}: only the SMU and preflight kinds are left out of the log"
        );
    }
    for kind in SILENT.iter().chain(UNLISTED) {
        assert!(matches!(words(*kind).history, History::Silent), "{kind}");
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
    assert_eq!(tag_word(&crash("boot", None).into()), "crash");
}

#[test]
fn the_history_line_of_a_kind_is_its_tag() {
    let mut h = Hist::three();
    h.push(crash("boot", None).into());
    let (entries, _) = h.facts.history.recent();
    assert_eq!(entries[0].tag, Tag::Crash);
}

/// The history line a kind words `payload` with, read back as the sentence its history entry shows.
fn history_line(payload: &Payload) -> Option<crate::words::Line> {
    let History::Line(of) = words(payload.kind()).history else {
        panic!("{} has no line of its own", payload.kind());
    };
    let line = of(payload)?;
    let entry = Entry::line(at(seq(1)), line.tag, line.text, line.tone);
    let (text, alarm, after) = entry.sentence();
    Some(crate::words::Line {
        tag: entry.tag,
        text: format!("{text}{alarm}{after}"),
        tone: entry.tone,
    })
}

fn said(payload: &Payload) -> (Tag, Tone, String) {
    let line = history_line(payload).unwrap_or_else(|| panic!("{} has no line", payload.kind()));
    (line.tag, line.tone, line.text)
}

fn ccd(id: i64) -> CcdId {
    CcdId::new(id).unwrap()
}

fn profile(offsets: &[(i64, i64)]) -> CcdProfile {
    let map: BTreeMap<_, _> = offsets.iter().map(|&(c, o)| (ccd(c), off(o))).collect();
    CcdProfile::try_from(map).unwrap()
}

pub(super) fn vector(offsets: &[(i64, i64)]) -> CcdVector {
    CcdVector {
        ccd: profile(offsets),
        cores: PerCore::default(),
    }
}

fn observed(offsets: &[(i64, i64)]) -> ObservedProfile {
    ObservedProfile::Ccd {
        ccd: profile(offsets),
        cores: PerCore::default(),
    }
}

fn class(regime: Regime) -> ExposureClass {
    ExposureClass {
        regime,
        workloads: Vec::new(),
        cores: set(&[0, 1]),
        position: None,
        duration_s: 120,
        placement: Placement::Isolated,
    }
}

fn boot_id() -> DayEventId {
    DayEventId::Boot {
        boot: BootId::from("0123456789abcdef".to_owned()),
    }
}

fn moment() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

pub(super) fn edge_event(old: Edge, new: Edge) -> Payload {
    CcdEdge {
        ccd: ccd(1),
        edge: new,
        old,
        reason: String::new(),
    }
    .into()
}

fn observed_edge(offset: i64) -> Edge {
    Edge::Observed {
        offset: off(offset),
        witness: seq(3),
    }
}

pub(super) fn cap_event(binding: Binding, slope: Slope) -> Payload {
    CcdCap {
        ccd: ccd(0),
        old: off(0),
        new: off(-15),
        budget: Rate::new(1.0).unwrap(),
        edge: Edge::Unknown {},
        adequate: Vec::new(),
        uncovered: Vec::new(),
        cells: Vec::new(),
        slope,
        candidates: Vec::new(),
        binding,
    }
    .into()
}

pub(super) fn plan_event() -> Payload {
    plan(None)
}

fn plan(night: Option<NightId>) -> Payload {
    NightPlan {
        night,
        budgets: vec![
            Budget {
                queue: Queue::Mapping,
                share_pct: 10,
                consumed_s: 3600,
            },
            Budget {
                queue: Queue::Qualification,
                share_pct: 75,
                consumed_s: 3600,
            },
        ],
        tasks: vec![
            PlannedTask {
                task: TaskId {
                    queue: Queue::Mapping,
                    ordinal: 1,
                },
                status: TaskStatus::Pending,
            };
            2
        ],
        unavailable: Vec::new(),
    }
    .into()
}

pub(super) fn qualification_event(state: QualificationState) -> Payload {
    NightQualification {
        profile: vector(&[(0, -10), (1, -5)]),
        firmware: togi_machine::BiosContext::default(),
        epoch: 1,
        contents: Vec::new(),
        matrix: Digest::from_bytes([7; 32]),
        purpose: QualificationPurpose::NextStep,
        state,
    }
    .into()
}

fn reproduction_event(state: ReproductionState) -> Payload {
    NightReproduction {
        source: boot_id(),
        target: Some(ReproductionTarget::Isolated {
            ccd: ccd(1),
            offset: off(-20),
        }),
        night: NightId(seq(4)),
        consumed_s: 0,
        remaining: Vec::new(),
        failures: Vec::new(),
        state,
    }
    .into()
}

fn day_event(classification: Classification, resolution: Resolution) -> Payload {
    DayEvent {
        event: boot_id(),
        profile: observed(&[(0, -10), (1, -5)]),
        boot: None,
        from: moment(),
        to: moment(),
        witnesses: Vec::new(),
        classification,
        resolution,
        reason: String::new(),
        predecessor: None,
        escalation: None,
        cursors: Vec::new(),
        consumed: Vec::new(),
        reproduction: ReproductionState::Pending,
    }
    .into()
}

pub(super) fn step_event(step: StepKind) -> Payload {
    DayStep {
        decision: DecisionId(1),
        cold_boot: BootId::from("b".to_owned()),
        old: vector(&[(0, -10), (1, -5)]),
        proposed: vector(&[(0, -15), (1, -5)]),
        step,
        predecessor: None,
        hours: Vec::new(),
        hold_s: 0,
        causes: StepCauses::default(),
        reason: String::new(),
    }
    .into()
}

pub(super) fn hold_event(change: HoldChange) -> Payload {
    DayHold {
        profile: vector(&[(0, -10)]),
        application: ApplicationId(1),
        credited: Vec::new(),
        eligible_s: 7200,
        change,
    }
    .into()
}

#[test_case(Edge::Unknown {}, observed_edge(-23), Tone::Warn, "CCD1 edge unknown → fails at -23"; "observed")]
#[test_case(
    observed_edge(-23),
    Edge::Censored { limit: off(-50), witnesses: Vec::new() },
    Tone::Plain,
    "CCD1 edge fails at -23 → censored at -50";
    "censored"
)]
#[test_case(observed_edge(-23), Edge::Unknown {}, Tone::Plain, "CCD1 edge fails at -23 → unknown"; "forgotten")]
fn an_edge_change_names_both_edges(old: Edge, new: Edge, tone: Tone, text: &str) {
    assert_eq!(
        said(&edge_event(old, new)),
        (Tag::Edge, tone, text.to_owned())
    );
}

#[test]
fn edges_read_as_the_dashboard_says_them() {
    assert_eq!(edge_words(&Edge::Unknown {}), "unknown");
    assert_eq!(edge_words(&observed_edge(-23)), "fails at -23");
}

#[test_case(Binding::Rate { class: class(Regime::R2) }, Slope::Fallback { beta: Finite::new(0.2).unwrap() }, "CCD0 cap 0 → -15 · rate of R2 heavy vector · fallback slope"; "rate and fallback")]
#[test_case(
    Binding::EdgeMargin {},
    Slope::Measured { lower: togi_journal::LowerEndpoint::Unbounded {}, historical_min: None },
    "CCD0 cap 0 → -15 · edge margin · measured slope";
    "edge margin and measured"
)]
#[test_case(Binding::Baseline {}, Slope::Fallback { beta: Finite::new(0.2).unwrap() }, "CCD0 cap 0 → -15 · no cap yet · fallback slope"; "baseline")]
fn a_cap_names_what_fixed_it_and_its_slope(binding: Binding, slope: Slope, text: &str) {
    assert_eq!(
        said(&cap_event(binding, slope)),
        (Tag::Cap, Tone::Plain, text.to_owned())
    );
}

#[test_case(None, "night opened · 2 tasks"; "the opening plan")]
#[test_case(Some(NightId(seq(4))), "night replanned · 2h00m elapsed · 2 tasks"; "a replan")]
fn a_night_plan_says_whether_it_opens_the_night_the_time_observed_and_the_tasks(
    night: Option<NightId>,
    text: &str,
) {
    assert_eq!(
        said(&plan(night)),
        (Tag::Night, Tone::Plain, text.to_owned())
    );
}

#[test]
fn a_qualification_speaks_when_it_starts_ends_or_fails_and_not_between() {
    let cell = || class(Regime::R7);
    let start = qualification_event(QualificationState::Start {
        outstanding: vec![cell(), cell()],
    });
    assert_eq!(
        said(&start),
        (
            Tag::Qualify,
            Tone::Plain,
            "qualifying CCD0 -10, CCD1 -5 (next step) · 2 cells to witness".to_owned()
        )
    );
    let progress = qualification_event(QualificationState::Progress {
        witnesses: Vec::new(),
        outstanding: vec![cell()],
    });
    assert_eq!(history_line(&progress), None);
    let done = qualification_event(QualificationState::Complete {
        witnesses: vec![togi_journal::CellWitness {
            cell: cell(),
            evidence: togi_journal::CellEvidence::Fresh { exposure: seq(2) },
        }],
    });
    assert_eq!(
        said(&done),
        (
            Tag::Qualify,
            Tone::Good,
            "qualified CCD0 -10, CCD1 -5 (next step) · 1 cell witnessed".to_owned()
        )
    );
    let failed = qualification_event(QualificationState::Failed {
        failure: seq(4),
        witnesses: Vec::new(),
        outstanding: vec![cell()],
    });
    assert_eq!(
        said(&failed),
        (
            Tag::Qualify,
            Tone::Bad,
            "qualification of CCD0 -10, CCD1 -5 (next step) failed · 1 cell outstanding".to_owned()
        )
    );
}

#[test_case(ReproductionState::Pending, None; "pending")]
#[test_case(
    ReproductionState::Reproduced,
    Some((Tone::Bad, "boot 01234567 reproduced on CCD1 alone at -20"));
    "reproduced"
)]
#[test_case(
    ReproductionState::Unreproduced,
    Some((Tone::Good, "boot 01234567 not reproduced on CCD1 alone at -20"));
    "unreproduced"
)]
fn a_reproduction_speaks_once_it_has_an_answer(
    state: ReproductionState,
    want: Option<(Tone, &str)>,
) {
    let got = history_line(&reproduction_event(state));
    assert_eq!(
        got.map(|line| (line.tag, line.tone, line.text)),
        want.map(|(tone, text)| (Tag::Repro, tone, text.to_owned()))
    );
}

#[test]
fn a_nonuniform_failure_is_untestable_and_says_no_night_writes_its_profile() {
    let Payload::NightReproduction(mut untestable) =
        reproduction_event(ReproductionState::Untestable)
    else {
        unreachable!("reproduction_event builds a reproduction");
    };
    untestable.target = None;
    assert_eq!(
        said(&untestable.into()),
        (
            Tag::Repro,
            Tone::Warn,
            "boot 01234567 cannot be retested: no profile a night writes".to_owned()
        )
    );
}

#[test_case(
    Classification::CpuSuspect { certainty: Certainty::Definite },
    Resolution::Unresolved {},
    Tone::Bad,
    "boot 01234567 at CCD0 -10, CCD1 -5 · CPU-suspect, unresolved";
    "suspect"
)]
#[test_case(
    Classification::CpuSuspect { certainty: Certainty::Provisional },
    Resolution::Resolved { retreat: None },
    Tone::Bad,
    "boot 01234567 at CCD0 -10, CCD1 -5 · provisionally CPU-suspect, resolved";
    "provisional"
)]
#[test_case(
    Classification::NonCpu {},
    Resolution::Resolved { retreat: None },
    Tone::Plain,
    "boot 01234567 at CCD0 -10, CCD1 -5 · not CPU, resolved";
    "not cpu"
)]
#[test_case(
    Classification::Unexplained {},
    Resolution::Unresolved {},
    Tone::Warn,
    "boot 01234567 at CCD0 -10, CCD1 -5 · unexplained, unresolved";
    "unexplained"
)]
fn a_daily_event_names_its_boot_profile_and_class(
    classification: Classification,
    resolution: Resolution,
    tone: Tone,
    text: &str,
) {
    assert_eq!(
        said(&day_event(classification, resolution)),
        (Tag::Day, tone, text.to_owned())
    );
}

#[test_case(VetoState::Active, Tone::Warn, "veto on CCD0 -10 from boot 01234567"; "active")]
#[test_case(VetoState::Cleared, Tone::Plain, "veto on CCD0 -10 cleared, from boot 01234567"; "cleared")]
fn a_veto_says_its_profile_and_source(state: VetoState, tone: Tone, text: &str) {
    let veto: Payload = DayVeto {
        event: boot_id(),
        profile: observed(&[(0, -10)]),
        state,
        evidence: Vec::new(),
        reason: String::new(),
    }
    .into();
    assert_eq!(said(&veto), (Tag::Veto, tone, text.to_owned()));
}

#[test]
fn a_hold_proposal_is_silent_and_a_move_is_not() {
    let hold = step_event(StepKind::Hold {
        blocked_by: vec![HoldReason::AtCap {}],
    });
    assert_eq!(history_line(&hold), None);
    assert_eq!(
        said(&step_event(StepKind::Promote { changed: ccd(0) })),
        (
            Tag::Step,
            Tone::Good,
            "promote CCD0 to CCD0 -15, CCD1 -5".to_owned()
        )
    );
    let retreat = step_event(StepKind::Retreat {
        changed: vec![ccd(0), ccd(1)],
        cause: togi_journal::RetreatCause::ShallowerCap {},
        wanted: None,
    });
    assert_eq!(
        said(&retreat),
        (
            Tag::Step,
            Tone::Warn,
            "retreat CCD0, CCD1 to CCD0 -15, CCD1 -5".to_owned()
        )
    );
    let firmware = step_event(StepKind::FirmwareZero {
        changed: vec![ccd(1)],
    });
    assert_eq!(
        said(&firmware),
        (
            Tag::Step,
            Tone::Warn,
            "firmware changed: CCD1 to CCD0 -15, CCD1 -5".to_owned()
        )
    );
}

#[test]
fn an_applied_profile_is_named_by_its_ccds() {
    let applied: Payload = DayApplied {
        application: ApplicationId(1),
        cause: ApplyCause::Reapplication {},
        target: vector(&[(0, -10), (1, -5)]),
        readbacks: PerCore::default(),
        transactions: Vec::new(),
        boot: BootId::from("b".to_owned()),
        mono_ns: 0,
    }
    .into();
    assert_eq!(
        said(&applied),
        (
            Tag::Apply,
            Tone::Plain,
            "applied day profile CCD0 -10, CCD1 -5".to_owned()
        )
    );
}

#[test_case(HoldChange::Credited {}, None; "credited hours add no line")]
#[test_case(
    HoldChange::Reset { cause: seq(2) },
    Some("hold at CCD0 -10 reset · 2h00m eligible");
    "reset"
)]
#[test_case(
    HoldChange::Recomputed { cause: seq(2) },
    Some("hold at CCD0 -10 recomputed · 2h00m eligible");
    "recomputed"
)]
fn a_hold_speaks_when_its_credit_resets_or_is_recomputed(change: HoldChange, want: Option<&str>) {
    let got = history_line(&hold_event(change));
    assert_eq!(
        got.map(|line| (line.tag, line.text)),
        want.map(|text| (Tag::Hold, text.to_owned()))
    );
}

#[test_case(FreezeChoice::Freeze, "promotion frozen"; "freeze")]
#[test_case(FreezeChoice::Unfreeze, "promotion unfrozen"; "unfreeze")]
fn a_freeze_says_which_way_it_went(choice: FreezeChoice, text: &str) {
    let freeze: Payload = DayFreeze {
        choice,
        source: FreezeSource::Command { uid: 0 },
    }
    .into();
    assert_eq!(said(&freeze), (Tag::Freeze, Tone::Plain, text.to_owned()));
}

#[test]
fn an_import_counts_what_it_took() {
    let import: Payload = SessionImported {
        sources: vec![
            togi_journal::CarriedSource {
                session: "20231114T221320Z".parse().unwrap(),
                schema: 1,
                ruleset: 12,
            },
            togi_journal::CarriedSource {
                session: "20231115T221320Z".parse().unwrap(),
                schema: 1,
                ruleset: 12,
            },
        ],
        selected: Vec::new(),
        counts: ImportCounts {
            selected: 9,
            imported: 7,
            duplicates: 1,
            unknown: 1,
            excluded: 0,
        },
        unknown: Vec::new(),
        excluded: Vec::new(),
        reason: String::new(),
    }
    .into();
    assert_eq!(
        said(&import),
        (
            Tag::Import,
            Tone::Plain,
            "imported 7 of 9 selected records from 2 sessions".to_owned()
        )
    );
}

#[test]
fn every_ruleset13_kind_with_words_is_tagged_in_the_log_as_its_line_is() {
    for &(kind, tag) in RULESET13 {
        let table = words(kind);
        assert!(matches!(table.history, History::Line(_)), "{kind}");
        assert!(table.listed, "{kind}");
        assert!(
            matches!(table.tag, Tagging::Fixed(fixed) if fixed == tag),
            "{kind}"
        );
    }
}

#[test]
fn the_ruleset13_kinds_all_have_a_line_or_are_named_silent() {
    let ruleset13 = [
        EventKind::ExposureObserved,
        EventKind::CcdEdge,
        EventKind::CcdCap,
        EventKind::NightPlan,
        EventKind::NightQualification,
        EventKind::NightReproduction,
        EventKind::DayEvent,
        EventKind::DayVeto,
        EventKind::DayInterval,
        EventKind::DayHold,
        EventKind::DayStep,
        EventKind::DayApplied,
        EventKind::DayFreeze,
        EventKind::DayImported,
        EventKind::SessionImported,
    ];
    for kind in ruleset13 {
        let spoken = RULESET13.iter().any(|&(spoken, _)| spoken == kind);
        assert_eq!(spoken, !SILENT.contains(&kind), "{kind}");
    }
}

fn historical_payloads() -> Vec<Payload> {
    let decision = togi_journal::TunerDecision {
        core: core(3),
        activity: togi_journal::Activity::Checking,
        decision: togi_journal::Decision::Backoff,
        from_offset: off(-10),
        to_offset: off(-15),
        pass: None,
        failure_point: None,
        workloads: Vec::new(),
        reason: String::new(),
    };
    vec![
        decision.into(),
        transition(3, Start::search(-10)).into(),
        cycle(1, togi_journal::CycleEvent::Start).into(),
        hunt_start(1, &[0, 2]).into(),
    ]
}

#[test]
fn a_kind_of_an_earlier_ruleset_says_its_recorded_message_and_nothing_else() {
    for payload in historical_payloads() {
        assert!(HISTORICAL.contains(&payload.kind()), "{}", payload.kind());
        let (tag, tone, text) = said(&payload);
        assert_eq!((tag, tone), (Tag::Note, Tone::Plain), "{}", payload.kind());
        assert_eq!(text, payload.message(), "{}", payload.kind());
        assert_eq!(
            tag_word(&payload),
            payload.kind().as_str().replace('.', " "),
            "the log names the kind"
        );
    }
    for &kind in HISTORICAL {
        assert!(matches!(words(kind).history, History::Line(_)), "{kind}");
        assert!(matches!(words(kind).tag, Tagging::Kind), "{kind}");
        assert!(words(kind).listed, "{kind}");
    }
}

#[test]
fn a_reset_command_says_its_message_under_the_reset_tag() {
    let reset = Payload::CommandReset(togi_journal::CommandReset {
        target: togi_journal::ResetTarget::Core(core(3)),
    });
    assert_eq!(said(&reset), (Tag::Reset, Tone::Plain, reset.message()));
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
fn a_failure_names_the_core_and_offset_or_the_idle_signal() {
    let mut named = failure(Signal::ComputationError, Some("t"));
    named.attribution = togi_journal::Attribution::Attributed;
    named.core = Some(core(3));
    named.offset = Some(off(-20));
    assert_eq!(
        failure_line(&named).map(|line| (line.tag, line.text)),
        Some((Tag::Fail, "core 03 fails at -20".to_owned()))
    );
    let idle = failure(Signal::Stall, None);
    assert_eq!(
        failure_line(&idle).map(|line| line.text),
        Some("idle with the offsets applied, the backend stalled".to_owned())
    );
    assert_eq!(failure_line(&failure(Signal::Crash, None)), None);
}

#[test_case(Regime::R1, "light load", "light"; "r1")]
#[test_case(Regime::R2, "heavy vector load", "heavy vector"; "r2")]
#[test_case(Regime::R3, "load steps", "load steps"; "r3")]
#[test_case(Regime::R4, "partial load", "partial load"; "r4")]
#[test_case(Regime::R5, "both threads", "both threads"; "r5")]
#[test_case(Regime::R6, "idle with short bursts", "idle + bursts"; "r6")]
#[test_case(Regime::R7, "all-core load", "all-core"; "r7")]
fn the_regime_words(regime: Regime, sentence: &str, kind: &str) {
    assert_eq!(regime_words(regime), sentence);
    assert_eq!(kind_words(regime), kind);
}

#[test]
fn every_regime_has_load_words() {
    for regime in togi_machine::ALL_REGIMES {
        assert!(!load_words(regime).is_empty(), "{regime:?}");
    }
    assert_eq!(load_words(Regime::R6), "idle + bursts");
}

#[test_case(Signal::ComputationError, "wrong result"; "ComputationError")]
#[test_case(Signal::UnexpectedExit, "the backend quit"; "UnexpectedExit")]
#[test_case(Signal::Stall, "the backend stalled"; "Stall")]
#[test_case(Signal::CorrectedMce, "corrected hardware error"; "CorrectedMce")]
#[test_case(Signal::UncorrectedMce, "uncorrected hardware error"; "UncorrectedMce")]
#[test_case(Signal::Crash, "crash"; "Crash")]
fn the_signal_words(signal: Signal, words: &str) {
    assert_eq!(signal_words(signal), words);
}

#[test]
fn counting_words() {
    assert_eq!(
        (noun(1, "step"), noun(2, "step"), noun(2, "crash")),
        ("step".into(), "steps".into(), "crashes".into())
    );
    assert_eq!(noun(2, "wish"), "wishes");
    assert_eq!(plural(0, "task"), "0 tasks");
    assert_eq!(plural(1, "cell"), "1 cell");
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
    for regime in togi_machine::ALL_REGIMES {
        for entry in togi_machine::workloads(regime) {
            let shown = workload_display(entry.id());
            assert!(!shown.is_empty(), "{regime:?} workload {:?}", entry.id());
            if !entry.base.is_empty() {
                assert_eq!(
                    shown,
                    togi_machine::workload_by_id(entry.base).unwrap().label,
                    "{regime:?} workload {:?}",
                    entry.id()
                );
            }
        }
    }
}

#[test]
fn a_re_qualification_event_on_a_per_core_profile_names_its_night_and_every_core() {
    let Payload::DayEvent(mut event) =
        day_event(Classification::Unexplained {}, Resolution::Unresolved {})
    else {
        unreachable!("day_event builds a daily event");
    };
    event.event = DayEventId::Requalification {
        night: NightId(seq(4)),
    };
    event.profile = ObservedProfile::PerCore {
        cores: [
            (core(0), togi_machine::ObservedOffset(-10)),
            (core(1), togi_machine::ObservedOffset(-5)),
        ]
        .into_iter()
        .collect(),
    };
    let (tag, tone, text) = said(&event.into());
    assert_eq!((tag, tone), (Tag::Day, Tone::Warn));
    assert!(
        text.starts_with("re-qualification of night #4 at per-core [core 00: -10, core 01: -5"),
        "{text}"
    );
    assert!(text.ends_with(" · unexplained, unresolved"), "{text}");
}

#[test]
fn a_re_qualification_says_what_it_qualifies_for() {
    let Payload::NightQualification(mut qualification) =
        qualification_event(QualificationState::Start {
            outstanding: Vec::new(),
        })
    else {
        unreachable!("qualification_event builds a qualification");
    };
    qualification.purpose = QualificationPurpose::Requalification;
    let (_, _, text) = said(&qualification.into());
    assert_eq!(
        text,
        "qualifying CCD0 -10, CCD1 -5 (re-qualification) · 0 cells to witness"
    );
}
