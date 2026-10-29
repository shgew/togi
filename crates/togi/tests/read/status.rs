use std::collections::BTreeMap;

use jiff::SignedDuration;
use togi_journal::fs::Fs as _;
use togi_journal::{
    ApplicationId, ApplyCause, Binding, Budget, CcdCap, CcdEdge, CcdVector, Classification,
    DayApplied, DayBootStart, DayEvent, DayEventId, DayFreeze, DayHold, DayStep, DayVeto,
    DecisionId, Edge, Event, FreezeChoice, FreezeSource, HoldChange, HoldReason, NightPlan,
    ObservedProfile, Payload, PlannedTask, Queue, ReproductionState, Resolution, Seq, Slope, Stamp,
    StepCauses, StepKind, TaskId, TaskStatus, VetoState,
};
use togi_journal::{
    Build, ConfigLoaded, DeadEnd, DeadEndCondition, FixStamp, SCHEMA, SessionBaseline,
    SessionContext, SessionStart,
};
use togi_journal::{
    Certainty, ExposureClass, Finite, Placement, Rate, UncoveredClass, UncoveredReason,
};
use togi_journal::{FactSource, Outcome, TrialCarried, TrialClass, TrialId, TrialIntent};
use togi_machine::{
    BiosContext, BootId, CcdId, CcdProfile, CoreId, CoreInfo, CoreSet, ObservedOffset, PerCore,
    Regime, TargetOffset,
};
use togi_watchtest::{Base, Scenario};
use unicode_width::UnicodeWidthStr as _;

use super::support::{Session, append_between_trial_mce, append_event, text, togi};

const _: () = assert!(Base::ALL.len() == 5, "every base needs a status sweep case");

/// Every scenario's golden and snapshot restore boundaries share its base simulation.
/// Separate exhaustive sweeps keep the five bases parallel without rebuilding any base.
#[test_case::test_case(Base::Night; "night")]
#[test_case::test_case(Base::Mapped; "mapped")]
#[test_case::test_case(Base::Filler; "filler")]
#[test_case::test_case(Base::Calendar; "calendar")]
#[test_case::test_case(Base::DeadEnd; "dead end")]
fn snapshot_plus_tail_renders_the_same_status_without_writes(base: Base) {
    for scenario in Scenario::ALL
        .into_iter()
        .filter(|scenario| scenario.base() == base)
    {
        let events = scenario
            .events()
            .unwrap_or_else(|error| panic!("{}: {error}", scenario.name()));
        let dir = super::support::installed(&events);
        let mutations = dir.fs.mutations();
        let ran = togi(&dir, &["status"]);
        assert_eq!(dir.fs.mutations(), mutations, "{}", scenario.name());
        assert_eq!(ran.status.code(), Some(0), "{}", scenario.name());
        assert_eq!(text(&ran.stderr), "", "{}", scenario.name());
        let shown = text(&ran.stdout);
        super::support::snapshot(&format!("status__status_{}", scenario.name()), shown);
        for line in shown.lines() {
            assert!(
                line.width() <= 80,
                "{}: {} cells: {line}",
                scenario.name(),
                line.width()
            );
        }
        for removed in [
            "phase 1",
            "phase 2",
            "search:",
            "checking cycle",
            "hunt",
            "combination",
            "R7 self-sufficiency",
            "SOLO",
            "unconfirmed",
            "--core",
            "night until",
            "Night until",
            "RESERVED",
        ] {
            assert!(
                !shown.contains(removed),
                "{}: {removed} in\n{shown}",
                scenario.name()
            );
        }
        // Installation encoded the journal once. Walk its line boundaries once too,
        // rather than encoding the overlapping snapshot prefixes again.
        let bytes = dir.fs.read(&super::support::journal(&dir)).unwrap();
        let mut lines = bytes.split_inclusive(|byte| *byte == b'\n');
        let mut previous_cut = 0;
        let mut byte_end = 0;
        for cut in [1, events.len() / 2, events.len()] {
            let state = togi_session::ReadState::replay(&events[..cut])
                .unwrap_or_else(|error| panic!("{} at {cut}: {error}", scenario.name()));
            byte_end += lines
                .by_ref()
                .take(cut - previous_cut)
                .map(<[u8]>::len)
                .sum::<usize>();
            previous_cut = cut;
            let prefix =
                togi_journal::snapshot::Prefix::of(&bytes[..byte_end], Some(events[cut - 1].seq));
            let saved = togi_journal::snapshot::seal(prefix, &state.document()).unwrap();
            dir.fs.put(
                dir.path().join("state.json"),
                serde_json::to_vec(&saved).unwrap(),
            );
            let mutations = dir.fs.mutations();
            let actual = togi(&dir, &["status"]);
            assert_eq!(actual.status, ran.status, "{} at {cut}", scenario.name());
            assert_eq!(actual.stdout, ran.stdout, "{} at {cut}", scenario.name());
            assert_eq!(actual.stderr, ran.stderr, "{} at {cut}", scenario.name());
            assert_eq!(
                dir.fs.mutations(),
                mutations,
                "{} at {cut}",
                scenario.name()
            );
        }
    }
}

#[test]
fn a_journal_without_a_session_is_an_error() {
    let dir = super::support::Session::empty();
    super::support::write_journal(&dir, "");
    let ran = togi(&dir, &["status"]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(&ran.stdout), "");
    assert!(
        text(&ran.stderr).contains("no session in"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn a_missing_journal_is_an_error_naming_the_file() {
    let dir = super::support::Session::empty();
    let ran = togi(&dir, &["status"]);
    assert_eq!(ran.status.code(), Some(1));
    assert!(
        text(&ran.stderr).contains("no journal at"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn a_malformed_journal_line_prints_its_path_line_and_parser_cause() {
    let dir = hand();
    let header = super::support::lines(&dir).remove(0);
    super::support::write_journal(&dir, format!("{header}\nnot an event\n"));
    let ran = togi(&dir, &["status"]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(&ran.stdout), "");
    assert_eq!(
        text(&ran.stderr),
        format!(
            "togi status: {}: line 2: malformed event: expected ident at line 1 column 2\n",
            dir.path().join("events.jsonl").display()
        )
    );
}

#[test]
fn status_takes_no_arguments() {
    let dir = hand();
    let ran = togi(&dir, &["status", "extra"]);
    assert_eq!(ran.status.code(), Some(2));
    assert_eq!(text(&ran.stdout), "");
}

#[test]
fn status_help() {
    let ran = togi(&super::support::Session::empty(), &["status", "--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    super::support::snapshot("status__status_help", text(&ran.stdout));
}

#[test]
fn a_between_trial_mce_is_listed_as_evidence_and_escaped() {
    let dir = hand();
    append_between_trial_mce(&dir);
    let ran = togi(&dir, &["status"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    let shown = text(&ran.stdout);
    assert!(shown.contains("between-trial evidence [#"), "{shown}");
    assert!(shown.contains("\\x1b[2J"), "{shown}");
    assert!(!shown.contains('\u{1b}'));
}

#[test]
fn wide_bios_fields_wrap_by_terminal_cells_and_stay_whole() {
    let dir = hand();
    let board = "日本 ".repeat(20);
    append_event(
        &dir,
        "session.context",
        "context",
        &serde_json::json!({
            "bios_version": "v".repeat(30), "board": board, "cpu_model": "cpu ".repeat(15),
            "microcode": "0x1", "boost_limit_mhz": 5000,
        }),
    );
    let shown = status_of(&dir);
    assert!(flat(&shown).contains(board.trim_end()), "{shown}");
    for line in shown.lines() {
        assert!(line.width() <= 80, "{} cells: {line}", line.width());
    }
}

#[test]
fn a_journal_the_fold_rejects_is_an_error_with_nothing_on_stdout() {
    let dir = hand();
    let mut lines = super::support::lines(&dir);
    let mut start: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(start["kind"], "session.start");
    start["data"]["cores"].as_array_mut().unwrap().pop();
    lines[0] = start.to_string();
    let line = lines
        .iter_mut()
        .find(|line| line.contains(r#""kind":"session.baseline""#))
        .unwrap();
    let mut baseline: serde_json::Value = serde_json::from_str(line).unwrap();
    baseline["data"]["offsets"] = serde_json::json!({"15": 0});
    *line = baseline.to_string();
    super::support::write_journal(&dir, lines.join("\n") + "\n");
    let ran = togi(&dir, &["status"]);
    assert_eq!(ran.status.code(), Some(1), "{}", text(&ran.stdout));
    assert_eq!(text(&ran.stdout), "");
    let stderr = text(&ran.stderr);
    assert!(
        stderr.starts_with("togi status: ") && stderr.contains("does not have"),
        "{stderr}"
    );
}

#[test]
fn a_failed_write_to_stdout_is_an_error() {
    let dir = hand();
    let ran = super::support::output(dir.host().run_to_closed_stdout(dir.path(), &["status"]));
    assert_eq!(ran.status.code(), Some(1), "{}", text(&ran.stderr));
    assert!(
        text(&ran.stderr).starts_with("togi status: write output: "),
        "{}",
        text(&ran.stderr)
    );
}

pub(super) fn installed(events: &[Event]) -> Session {
    super::support::installed(events)
}

fn status_of(dir: &Session) -> String {
    let ran = togi(dir, &["status"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stderr), "");
    text(&ran.stdout).to_owned()
}

/// `shown` with every run of whitespace as one space, so a wrapped sentence can be searched whole.
fn flat(shown: &str) -> String {
    shown.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn next_seq(events: &[Event]) -> Seq {
    Seq::new(events.last().unwrap().seq.get() + 1).unwrap()
}

/// `events` followed by `extra` payloads, one second apart, on the last event's boot.
fn append(events: &[Event], extra: Vec<Payload>) -> Vec<Event> {
    then(
        events,
        extra
            .into_iter()
            .map(|payload| (Vec::new(), payload))
            .collect(),
    )
}

/// `events` followed by `extra` payloads, each with the causes named, on the last event's boot.
pub(super) fn then(events: &[Event], extra: Vec<(Vec<Seq>, Payload)>) -> Vec<Event> {
    let mut events = events.to_vec();
    for (cause, payload) in extra {
        let last = events.last().unwrap();
        let stamp = Stamp {
            time: last
                .time
                .checked_add(jiff::SignedDuration::from_secs(1))
                .unwrap(),
            mono: None,
        };
        let event = Event::new(next_seq(&events), stamp, last.boot.clone(), cause, payload);
        events.push(event);
    }
    events
}

pub(super) fn last_intent(events: &[Event]) -> TrialIntent {
    events
        .iter()
        .rev()
        .find_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload {
                Some(intent.clone())
            } else {
                None
            }
        })
        .unwrap()
}

pub(super) fn fact_source(events: &[Event], trial: Option<&str>) -> FactSource {
    let Payload::SessionStart(start) = &events[0].payload else {
        unreachable!("a journal starts with its session");
    };
    FactSource {
        session: start.session,
        seq: Seq::new(40).unwrap(),
        build: start.build.clone(),
        trial: trial.map(|trial| TrialId::from(trial.to_owned())),
        time: events[0].time,
        boot: events[0].boot.clone(),
        backend: None,
    }
}

/// A carried pass of the class and profile `intent` ran.
pub(super) fn carried_pass(events: &[Event], intent: &TrialIntent) -> TrialCarried {
    TrialCarried {
        source: fact_source(events, Some("origin")),
        class: TrialClass {
            regime: intent.regime,
            workload: Some(intent.workload),
            cores: intent.cores,
            duration_s: intent.duration_s,
        },
        condition: intent.condition,
        activity: None,
        rerun: false,
        profile: intent.profile,
        outcome: Outcome::Pass,
        signal: None,
        duration_s: intent.duration_s,
        core: None,
        stalled_core: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
    }
}

/// A session with no trial: its start and its configuration, so the journal holds only what a test adds.
fn base() -> Vec<Event> {
    let start = Event::new(
        Seq::new(1).unwrap(),
        Stamp {
            time: "2026-01-01T00:00:00Z".parse().unwrap(),
            mono: None,
        },
        "boot-1".to_owned().into(),
        Vec::new(),
        SessionStart {
            build: build(),
            session: "20260101T000000Z".parse().unwrap(),
            cores: topology(),
        }
        .into(),
    );
    let events = vec![start];
    then(
        &events,
        vec![
            (
                Vec::new(),
                ConfigLoaded {
                    build: build(),
                    config: togi_config::Config::default().snapshot(),
                    path: "/etc/togi/config.json".to_owned(),
                    file: false,
                    kernel_cursor: None,
                    kernel_error: None,
                }
                .into(),
            ),
            (
                Vec::new(),
                SessionContext {
                    bios_version: "F1".to_owned(),
                    board: "board".to_owned(),
                    cpu_model: "CPU".to_owned(),
                    microcode: "0x1".to_owned(),
                    boost_limit_mhz: 5000,
                }
                .into(),
            ),
            (
                Vec::new(),
                SessionBaseline {
                    offsets: PerCore::default(),
                }
                .into(),
            ),
        ],
    )
}

fn build() -> Build {
    Build {
        version: "0.1.0".into(),
        rev: None,
        ruleset: togi_tuner::RULESET,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: togi_tuner::EVIDENCE_EPOCH,
    }
}

fn hand() -> Session {
    installed(&base())
}

fn assert_fits(shown: &str) {
    for line in shown.lines() {
        assert!(line.width() <= 80, "{} cells: {line}", line.width());
    }
}

fn ccd(id: i64) -> CcdId {
    CcdId::new(id).unwrap()
}

fn offset(value: i64) -> TargetOffset {
    TargetOffset::new(value).unwrap()
}

/// The 16 cores of the two CCDs, 8 each.
fn topology() -> Vec<CoreInfo> {
    (0..16)
        .map(|core| CoreInfo {
            core: CoreId::new(core).unwrap(),
            ccd: ccd(core / 8),
            cpus: Vec::new(),
        })
        .collect()
}

/// A day profile with the CCDs at the offsets given and CCD 1 at 0.
fn profile(first: i64, second: i64) -> CcdVector {
    let day = CcdProfile::zero([ccd(0), ccd(1)])
        .unwrap()
        .with(ccd(0), offset(first))
        .with(ccd(1), offset(second));
    CcdVector::expand(day, &topology()).unwrap()
}

/// A mapping class with no workload named, so the journal needs no workload content.
fn class(regime: Regime, duration_s: u32) -> ExposureClass {
    ExposureClass {
        regime,
        workloads: Vec::new(),
        cores: CoreSet::default(),
        position: None,
        duration_s,
        placement: Placement::Isolated,
    }
}

fn observed(vector: &CcdVector) -> ObservedProfile {
    ObservedProfile::Ccd {
        ccd: vector.ccd().clone(),
        cores: vector
            .cores()
            .iter()
            .map(|(core, offset)| (core, offset.observed()))
            .collect::<PerCore<ObservedOffset>>(),
    }
}

fn edge_and_cap(first: i64, cap: i64) -> Vec<Payload> {
    let edge = Edge::Observed {
        offset: offset(first),
        witness: Seq::new(1).unwrap(),
    };
    vec![
        CcdEdge {
            ccd: ccd(0),
            edge: edge.clone(),
            old: Edge::Unknown {},
            reason: "first failure".to_owned(),
        }
        .into(),
        CcdCap {
            ccd: ccd(0),
            old: TargetOffset::ZERO,
            new: offset(cap),
            budget: Rate::new(0.0001).unwrap(),
            edge,
            adequate: vec![class(Regime::R1, 90)],
            uncovered: vec![
                UncoveredClass {
                    class: class(Regime::R3, 120),
                    reason: UncoveredReason::NonMappingOnly,
                },
                UncoveredClass {
                    class: class(Regime::R3, 120),
                    reason: UncoveredReason::NonMappingOnly,
                },
                UncoveredClass {
                    class: class(Regime::R6, 900),
                    reason: UncoveredReason::NoObservations,
                },
            ],
            cells: Vec::new(),
            slope: Slope::Fallback {
                beta: Finite::new(0.405).unwrap(),
            },
            candidates: Vec::new(),
            binding: Binding::EdgeMargin {},
        }
        .into(),
    ]
}

/// The plan that opens a night, with mapping running and the control queue still to come.
fn night_plan() -> Payload {
    let budget = |queue, share_pct, consumed_s| Budget {
        queue,
        share_pct,
        consumed_s,
    };
    let task = |queue, status| PlannedTask {
        task: TaskId { queue, ordinal: 1 },
        status,
    };
    NightPlan {
        night: None,
        budgets: vec![
            budget(Queue::Mapping, 10, 3_600),
            budget(Queue::Qualification, 75, 10_800),
            budget(Queue::Control, 5, 0),
        ],
        tasks: vec![
            task(Queue::Mapping, TaskStatus::Running),
            task(Queue::Control, TaskStatus::Pending),
        ],
        unavailable: Vec::new(),
    }
    .into()
}

#[test]
fn a_historical_statistical_cap_is_not_presented_as_daily_permission() {
    let events = append(&base(), edge_and_cap(-20, -15));
    let raw = status_of(&installed(&events));
    assert_fits(&raw);
    let shown = flat(&raw);
    assert!(
        shown.contains("permission: experimental hold + exact night qualification"),
        "{shown}"
    );
    assert!(shown.contains("S: diagnostic, non-authorizing"), "{shown}");
    assert!(!shown.contains("D S SLOPE BINDING"), "{shown}");
    assert!(!shown.contains("remaining to S"), "{shown}");
}

#[test]
fn missing_campaign_history_is_explicit_not_an_unused_allowance() {
    let shown = flat(&status_of(&hand()));
    assert!(
        shown.contains("allowance: unavailable; campaign history must be reconstructed"),
        "{shown}"
    );
    assert!(!shown.contains("2 remaining"), "{shown}");
}

#[test]
fn an_open_night_names_its_start_elapsed_time_shares_and_current_task() {
    let events = append(&base(), vec![night_plan()]);
    let shown = status_of(&installed(&events));
    assert_fits(&shown);
    assert_eq!(
        shown.lines().next(),
        Some("night since 00:00, 4h00m elapsed: mapping (task mapping #1)"),
        "{shown}"
    );
    assert!(
        shown.contains("\nNight since 00:00\nelapsed: 4h00m\n"),
        "{shown}"
    );
    let flat = flat(&shown);
    assert!(flat.contains("QUEUE SHARE USED TIME"), "{flat}");
    assert!(flat.contains("mapping 10% 25% 1h00m"), "{flat}");
    assert!(flat.contains("qualification 75% 75% 3h00m"), "{flat}");
    assert!(flat.contains("control 5% 0% 0s"), "{flat}");
    assert!(
        flat.contains("current task: mapping (task mapping #1)"),
        "{flat}"
    );

    for gone in ["until", "RESERVED", "deadline"] {
        assert!(!shown.contains(gone), "{gone}: {shown}");
    }
}

fn shutdown() -> Payload {
    togi_journal::Shutdown {
        reason: togi_journal::ShutdownReason::Signal,
        kernel_cursor: None,
        kernel_error: None,
        cycles: None,
    }
    .into()
}

#[test_case::test_case(None; "before any night")]
#[test_case::test_case(Some(night_plan()); "after a night")]
fn a_shutdown_names_the_runtime_stop(night: Option<Payload>) {
    let events = append(&base(), night.into_iter().chain([shutdown()]).collect());
    let shown = status_of(&installed(&events));
    assert_eq!(shown.lines().next(), Some("stopped"), "{shown}");
}

#[test]
fn loading_configuration_reopens_a_runtime_stop() {
    let base = base();
    let events = append(&base, vec![shutdown(), base[1].payload.clone()]);
    let shown = status_of(&installed(&events));
    assert_eq!(shown.lines().next(), Some("day"), "{shown}");
}

#[test]
fn a_stock_policy_stop_is_not_a_runtime_stop_after_configuration_reloads() {
    let base = base();
    let events = append(
        &base,
        vec![
            DeadEnd {
                condition: DeadEndCondition::FailureAtZero,
                core: None,
                detail: "platform unstable at stock".to_owned(),
                action: None,
            }
            .into(),
            shutdown(),
            base[1].payload.clone(),
        ],
    );
    let model = togi_watch::model::project(&events);
    assert_eq!(
        (
            model.snapshot().policy.stopped,
            model.snapshot().stopped,
            model.journal_state().dead_end,
        ),
        (true, None, None)
    );
    let shown = status_of(&installed(&events));
    assert_eq!(shown.lines().next(), Some("day"), "{shown}");
}

#[test_case::test_case(None, "07:00"; "open night")]
#[test_case::test_case(Some(shutdown()), "00:00"; "stopped night")]
fn a_normal_boot_after_the_night_closes_it_and_status_shows_the_day_and_the_last_night(
    stop: Option<Payload>,
    closed: &str,
) {
    let events = append(&base(), vec![night_plan()]);
    let mut events = append(&events, stop.into_iter().collect());
    let last = events.last().unwrap();
    let time = last
        .time
        .checked_add(SignedDuration::from_hours(7))
        .unwrap();
    let start = Event::new(
        Seq::new(last.seq.get() + 1).unwrap(),
        Stamp { time, mono: None },
        "day-a".to_owned().into(),
        Vec::new(),
        DayBootStart {
            build: build(),
            bios: BiosContext::default(),
            offsets: PerCore::default(),
            watchdog: None,
        }
        .into(),
    );
    events.push(start);
    let shown = status_of(&installed(&events));
    assert_eq!(shown.lines().next(), Some("day"), "{shown}");
    assert!(
        shown.contains(&format!(
            "\nLast night 00:00\u{2013}{closed}\nelapsed: 4h00m\n"
        )),
        "{shown}"
    );
}

#[test]
fn a_dead_end_leads_the_header() {
    let events = append(
        &base(),
        vec![
            DeadEnd {
                condition: DeadEndCondition::NoEvidence,
                core: None,
                detail: "no backend".to_owned(),
                action: None,
            }
            .into(),
            shutdown(),
        ],
    );
    let shown = status_of(&installed(&events));
    assert_eq!(shown.lines().next(), Some("dead end"), "{shown}");
    assert!(
        shown.contains("\ndead end: no conclusive evidence [#5]\n"),
        "{shown}"
    );
}

#[test]
fn daily_safety_stays_visible_beside_experimental_permission() {
    let base = base();
    let boot: BootId = "day-a".to_owned().into();
    let vetoing = DayEventId::Boot { boot: boot.clone() };
    let current = profile(-10, -5);
    let events = append(
        &base,
        vec![
            DayApplied {
                application: ApplicationId(1),
                cause: ApplyCause::Reapplication {},
                target: current.clone(),
                readbacks: PerCore::default(),
                transactions: Vec::new(),
                boot: boot.clone(),
                mono_ns: 1,
            }
            .into(),
            DayHold {
                profile: current.clone(),
                application: ApplicationId(1),
                credited: Vec::new(),
                eligible_s: 45_000,
                change: HoldChange::Credited {},
            }
            .into(),
            DayEvent {
                event: vetoing.clone(),
                profile: observed(&profile(-12, -5)),
                boot: Some(boot.clone()),
                from: base.last().unwrap().time,
                to: base.last().unwrap().time,
                witnesses: Vec::new(),
                classification: Classification::CpuSuspect {
                    certainty: Certainty::Definite,
                },
                resolution: Resolution::Unresolved {},
                reason: "watchdog reset".to_owned(),
                predecessor: None,
                escalation: None,
                cursors: Vec::new(),
                consumed: Vec::new(),
                reproduction: ReproductionState::Pending,
            }
            .into(),
            DayVeto {
                event: vetoing.clone(),
                profile: observed(&profile(-12, -5)),
                state: VetoState::Active,
                evidence: Vec::new(),
                reason: "watchdog reset".to_owned(),
            }
            .into(),
            DayStep {
                decision: DecisionId(1),
                cold_boot: boot,
                old: current.clone(),
                proposed: current,
                step: StepKind::Hold {
                    blocked_by: vec![
                        HoldReason::Incomplete {
                            credited_s: 45_000,
                            required_s: 180_000,
                        },
                        HoldReason::Vetoed { veto: vetoing },
                        HoldReason::Frozen {},
                    ],
                },
                predecessor: None,
                hours: Vec::new(),
                hold_s: 45_000,
                causes: StepCauses::default(),
                reason: "hold".to_owned(),
            }
            .into(),
            DayFreeze {
                choice: FreezeChoice::Freeze,
                source: FreezeSource::Command { uid: 1000 },
            }
            .into(),
        ],
    );
    let shown = status_of(&installed(&events));
    let flat = flat(&shown);
    assert!(flat.contains("Day policy"), "{flat}");
    assert!(
        flat.contains("permission: experimental hold + exact night qualification"),
        "{flat}"
    );
    assert!(flat.contains("S: diagnostic, non-authorizing"), "{flat}");
    assert!(flat.contains("freeze: on"), "{flat}");
    assert!(
        flat.contains("event: boot day-a: CPU-suspect, unresolved, reproduction pending"),
        "{flat}"
    );
    assert!(flat.contains("veto: boot day-a: active"), "{flat}");
}

#[test]
fn a_defect_names_its_decisions_and_advises_a_full_reset_not_a_core_reset() {
    let dir = hand();
    append_event(
        &dir,
        "defect.found",
        "defect 1",
        &serde_json::json!({
            "id": 1, "title": "step\u{1b}[31m too cautious", "detail": "why", "pr": 7,
            "direction": "too_cautious", "cores": [3, 5], "decisions": [4, 6],
        }),
    );
    let shown = status_of(&dir);
    assert!(
        shown.contains(
            "defect 1: step\\x1b[31m too cautious (too cautious; fixed by pull request #7)"
        ),
        "{shown}"
    );
    assert!(
        shown.contains("decisions #4 #6 affected cores 03 05"),
        "{shown}"
    );
    assert!(shown.contains("  togi reset --all\n"), "{shown}");
    assert!(!shown.contains("--core"), "{shown}");
}

#[test]
fn a_too_aggressive_defect_is_listed_too() {
    let dir = hand();
    append_event(
        &dir,
        "defect.found",
        "defect 2",
        &serde_json::json!({
            "id": 2, "title": "too deep", "detail": "why", "pr": 9,
            "direction": "too_aggressive", "cores": [1], "decisions": [2],
        }),
    );
    let shown = status_of(&dir);
    assert!(
        shown.contains("defect 2: too deep (too aggressive;"),
        "{shown}"
    );
    assert!(shown.contains("togi reset --all"), "{shown}");
}

#[test]
fn an_answered_defect_stays_and_a_historical_core_reset_does_not_remove_it() {
    let dir = hand();
    append_event(
        &dir,
        "defect.found",
        "defect 1",
        &serde_json::json!({
            "id": 1, "title": "too cautious", "detail": "why", "pr": 7,
            "direction": "too_cautious", "cores": [3, 5], "decisions": [4, 6],
        }),
    );
    append_event(
        &dir,
        "defect.answered",
        "defect 1 answered no",
        &serde_json::json!({"id": 1, "cores": [3, 5], "answer": "no"}),
    );
    assert!(status_of(&dir).contains("defect 1: "));
    append_event(
        &dir,
        "command.reset",
        "reset core 3",
        &serde_json::json!({"target": {"core": 3}}),
    );
    assert!(status_of(&dir).contains("defect 1: "));
}

#[test]
fn a_reset_of_the_session_removes_the_defect() {
    let dir = hand();
    append_event(
        &dir,
        "defect.found",
        "defect 1",
        &serde_json::json!({
            "id": 1, "title": "too cautious", "detail": "why", "pr": 7,
            "direction": "too_cautious", "cores": [3, 5], "decisions": [4, 6],
        }),
    );
    append_event(
        &dir,
        "command.reset",
        "reset all",
        &serde_json::json!({"target": "all"}),
    );
    assert!(!status_of(&dir).contains("defect 1"));
}

const HOSTILE: &str = "a\u{1b}[31mb\u{202e}c\nd\u{2066}e";
const HOSTILE_ESCAPED: &str = "a\\x1b[31mb\\u202ec\\nd\\u2066e";

fn assert_visibly_escaped(shown: &str) {
    for raw in ['\u{1b}', '\u{202e}', '\u{2066}'] {
        assert!(
            !shown.contains(raw),
            "{raw:?} reached the terminal:\n{shown}"
        );
    }
}

#[test]
fn a_historical_carried_session_is_not_shown() {
    let dir = hand();
    append_event(
        &dir,
        "session.carried",
        HOSTILE,
        &serde_json::json!({"sources": [], "failure_points": false}),
    );
    let shown = status_of(&dir);
    assert_visibly_escaped(&shown);
    assert!(!shown.contains("carried"), "{shown}");
}

#[test]
fn controls_in_bios_context_fields_are_escaped() {
    let dir = hand();
    append_event(
        &dir,
        "session.context",
        "context",
        &serde_json::json!({
            "bios_version": HOSTILE, "board": HOSTILE, "cpu_model": HOSTILE,
            "microcode": HOSTILE, "boost_limit_mhz": 5000,
        }),
    );
    let shown = status_of(&dir);
    assert_visibly_escaped(&shown);
    assert_eq!(flat(&shown).matches(HOSTILE_ESCAPED).count(), 4, "{shown}");
}

fn evidence_output(scenario: Scenario, no_color: bool) -> String {
    evidence_in(&super::support::session(scenario), no_color)
}

fn evidence_in(state: &Session, no_color: bool) -> String {
    let mutations = state.fs.mutations();
    let mut host = state.host();
    host.stdout_facts = Some(togi_render::OutputFacts {
        is_terminal: true,
        no_color,
        journald: false,
    });
    let ran = super::support::output(host.run(state.path(), &["status", "--evidence"]));
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stderr), "");
    assert_eq!(state.fs.mutations(), mutations);
    text(&ran.stdout).to_owned()
}

#[test]
fn baseline_stock_does_not_mask_a_completed_night_profile_without_the_day_service() {
    let base = base();
    let Payload::SessionContext(context) = &base[2].payload else {
        unreachable!();
    };
    let qualification = togi_journal::NightQualification {
        profile: profile(-3, 0),
        firmware: context.bios_context(),
        epoch: togi_tuner::EVIDENCE_EPOCH,
        contents: Vec::new(),
        matrix: togi_machine::Digest::from_bytes([3; 32]),
        purpose: togi_journal::QualificationPurpose::NextStep,
        state: togi_journal::QualificationState::Start {
            outstanding: Vec::new(),
        },
    };
    let events = append(
        &base,
        vec![
            qualification.clone().into(),
            togi_journal::NightQualification {
                state: togi_journal::QualificationState::Complete {
                    witnesses: Vec::new(),
                },
                ..qualification
            }
            .into(),
        ],
    );
    let shown = flat(&evidence_in(&installed(&events), true));
    assert!(
        shown.contains(
            "Exact night-qualified profile (not applied day evidence): CCD 0 -3, CCD 1 0"
        ),
        "{shown}"
    );
    assert!(
        !shown.contains("no distinct nonzero selected profile"),
        "{shown}"
    );
}

#[test_case::test_case(Scenario::Controls; "no cap evidence")]
#[test_case::test_case(Scenario::Mapping; "mapping witnesses")]
#[test_case::test_case(Scenario::DayStep; "qualified profile without day application")]
fn evidence_is_read_only_and_identical_under_no_color(scenario: Scenario) {
    let shown = evidence_output(scenario, false);
    assert_eq!(shown, evidence_output(scenario, true));
    assert!(!shown.contains('\u{1b}'));
    assert!(shown.contains("Ruleset 14."));
    let shown_flat = flat(&shown);
    assert!(shown_flat.contains("Permission: experimental hold + exact night qualification"));
    assert!(shown_flat.contains("S is a non-authorizing diagnostic"));
    assert!(shown_flat.contains("Current exact-profile daily evidence alone"));
    assert!(shown.contains("cap calculation witnesses"));
    assert!(shown.contains("Qualification exposure"));
    assert!(shown.contains("Applied-step history"));
    assert!(shown.contains("Comparable same-class night clocks and temperatures"));
    assert!(shown.contains("stock (CO 0):"));
    for line in shown.lines() {
        assert!(line.width() <= 80, "{} cells: {line}", line.width());
    }
    super::support::snapshot(&format!("status__evidence_{}", scenario.name()), &shown);
}

fn morning_state() -> Session {
    let mut events = append(&base(), vec![night_plan()]);
    let last = events.last().unwrap();
    let start = Event::new(
        Seq::new(last.seq.get() + 1).unwrap(),
        Stamp {
            time: last
                .time
                .checked_add(SignedDuration::from_hours(7))
                .unwrap(),
            mono: None,
        },
        "morning".to_owned().into(),
        Vec::new(),
        DayBootStart {
            build: build(),
            bios: BiosContext::default(),
            offsets: PerCore::default(),
            watchdog: None,
        }
        .into(),
    );
    events.push(start);
    installed(&events)
}

#[test]
fn first_day_status_and_evidence_show_the_same_morning_report() {
    let state = morning_state();
    let status = togi(&state, &["status"]);
    let shown = text(&status.stdout);
    assert!(shown.contains("\nMorning report\n"), "{shown}");
    let evidence = evidence_in(&state, true);
    assert_eq!(evidence, evidence_in(&state, false));
    let morning = shown.split_once("\nMorning report\n").unwrap().1;
    assert!(evidence.contains(morning));
    super::support::snapshot("status__status_morning", shown);
    super::support::snapshot("status__evidence_morning", &evidence);
}

/// Explicit fixture publication only; ordinary tests stay on `MemFs`.
#[test]
#[ignore = "writes status fixtures to an explicitly supplied temporary directory"]
fn real_fs_generate_status_snapshots() {
    let directory = std::env::var_os("TOGI_STATUS_SNAPSHOTS").expect("set TOGI_STATUS_SNAPSHOTS");
    let directory = std::path::Path::new(&directory);
    assert!(
        directory.starts_with(std::env::temp_dir()),
        "use a temporary directory"
    );
    std::fs::create_dir_all(directory).unwrap();
    let emit = |name: &str, body: &str| {
        std::fs::write(
            directory.join(format!("read__status__{name}.snap")),
            format!("---\nsource: crates/togi/tests/read/status.rs\nexpression: text(&ran.stdout)\n---\n{body}"),
        ).unwrap();
    };
    let help = togi(&Session::empty(), &["status", "--help"]);
    emit("status_help", text(&help.stdout));
    for scenario in Scenario::ALL {
        let state = super::support::session(scenario);
        let ran = togi(&state, &["status"]);
        assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
        emit(&format!("status_{}", scenario.name()), text(&ran.stdout));
    }
    for scenario in [Scenario::Controls, Scenario::Mapping, Scenario::DayStep] {
        emit(
            &format!("evidence_{}", scenario.name()),
            &evidence_output(scenario, true),
        );
    }
    let morning = morning_state();
    emit("status_morning", text(&togi(&morning, &["status"]).stdout));
    emit("evidence_morning", &evidence_in(&morning, true));
}
