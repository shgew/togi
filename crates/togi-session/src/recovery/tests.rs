use std::time::Duration;

use jiff::Timestamp;
use proptest::prelude::*;
use togi_journal::{
    Activity, BackendRetry, BootLeaveReason, BootSavedEntry, Build, CarriedCore, CorePhase,
    CoreTransition, CrashDetected, DeadEnd, DeadEndCondition, DefectId, Event, EventKind, FixStamp,
    HuntEnd, HuntGroup, HuntOutcome, HuntStage, Mce, Outcome, Payload, ProfileApplied,
    ProfileChange, ProfileRestored, RetryTarget, SCHEMA, Seq, SessionBaseline, SessionCarried,
    SessionStart, SessionWarning, Shutdown, ShutdownReason, SmuError, SmuOp, SmuReadback, Stamp,
    TrialEnd, TrialId, TrialIntent, TrialProgress, TrialStart,
};
use togi_machine::{
    Backend, BankType, BootId, CcdId, Condition, CoreInfo, CoreSet, CpuId, KernelCursor,
    ObservedOffset, PerCore, Regime, ResetKind, Signal, pick_workload,
};

use super::{IntentKind, RecoveryState, TargetWrite};
use crate::error::FoldError;
use crate::testing::{build, core, target};

pub(crate) fn boot(name: &str) -> BootId {
    BootId::from(name.to_owned())
}

fn seq(n: u64) -> Seq {
    Seq::new(n).unwrap()
}

/// A journal written by hand, one event after another.
pub(crate) struct Log {
    pub(crate) events: Vec<Event>,
    boot: BootId,
    mono_ms: Option<u64>,
    wall: i64,
}

impl Log {
    /// A session over cores 0 and 8, on boot `b0`.
    pub(crate) fn new() -> Self {
        Self::on(&[(0, 0), (8, 1)])
    }

    pub(crate) fn on(cores: &[(i64, u8)]) -> Self {
        let mut log = Self {
            events: Vec::new(),
            boot: boot("b0"),
            mono_ms: Some(0),
            wall: 0,
        };
        log.add(
            Payload::SessionStart(SessionStart {
                build: build(),
                session: "20260101T000000Z".parse().unwrap(),
                cores: cores
                    .iter()
                    .map(|&(id, ccd)| CoreInfo {
                        core: core(id),
                        ccd: CcdId::try_from(ccd).unwrap(),
                        cpus: vec![CpuId::from(u16::try_from(id).unwrap())],
                    })
                    .collect(),
            }),
            &[],
        );
        log
    }

    pub(crate) fn at(&mut self, name: &str, mono_ms: u64) -> &mut Self {
        self.boot = boot(name);
        self.mono_ms = Some(mono_ms);
        self
    }

    pub(crate) fn mono(&mut self, mono_ms: u64) -> &mut Self {
        self.mono_ms = Some(mono_ms);
        self
    }

    fn wall(&mut self, seconds: i64) -> &mut Self {
        self.wall = seconds;
        self
    }

    pub(crate) fn next(&self) -> Seq {
        seq(u64::try_from(self.events.len()).unwrap() + 1)
    }

    fn stamp(&self) -> Stamp {
        Stamp {
            time: Timestamp::from_second(1_767_225_600 + self.wall).unwrap(),
            mono: self.mono_ms.map(Duration::from_millis),
        }
    }

    pub(crate) fn add(&mut self, payload: Payload, causes: &[Seq]) -> Seq {
        let seq = self.next();
        self.events.push(Event::new(
            seq,
            self.stamp(),
            self.boot.clone(),
            causes.to_vec(),
            payload,
        ));
        seq
    }

    /// An event only the journal can write, decoded from its line.
    fn smu(&mut self, kind: &str, data: &str, causes: &[Seq]) -> Seq {
        let seq = self.next();
        let causes: Vec<String> = causes.iter().map(ToString::to_string).collect();
        let time = self.stamp().time;
        let mono = self
            .mono_ms
            .map_or_else(String::new, |ms| format!(r#""mono_ms":{ms},"#));
        let line = format!(
            r#"{{"seq":{seq},"time":"{time}","boot":"{}",{mono}"cause":[{}],"kind":"{kind}","msg":"m","data":{data}}}"#,
            self.boot,
            causes.join(",")
        );
        self.events.push(Event::decode(&line).unwrap());
        seq
    }

    pub(crate) fn intent(&mut self, core: i64, offset: i64, causes: &[Seq]) -> Seq {
        self.smu(
            "smu.intent",
            &format!(r#"{{"op":"set","core":{core},"offset":{offset}}}"#),
            causes,
        )
    }

    pub(crate) fn intent_all(&mut self, offset: i64, causes: &[Seq]) -> Seq {
        self.smu(
            "smu.intent",
            &format!(r#"{{"op":"set_all","offset":{offset}}}"#),
            causes,
        )
    }

    pub(crate) fn written(&mut self, core: i64, offset: i64, intent: Seq) -> Seq {
        self.smu(
            "smu.write",
            &format!(r#"{{"op":"set","core":{core},"offset":{offset}}}"#),
            &[intent],
        )
    }

    pub(crate) fn readback(
        &mut self,
        core_id: i64,
        observed: i64,
        expected: i64,
        write: Seq,
    ) -> Seq {
        self.add(
            Payload::SmuReadback(SmuReadback {
                core: core(core_id),
                offset: ObservedOffset(observed),
                expected: Some(target(expected)),
            }),
            &[write],
        )
    }

    pub(crate) fn baseline(&mut self, offsets: &[(i64, i64)]) -> Seq {
        self.add(
            Payload::SessionBaseline(SessionBaseline {
                offsets: offsets
                    .iter()
                    .map(|&(id, offset)| (core(id), ObservedOffset(offset)))
                    .collect(),
            }),
            &[],
        )
    }

    pub(crate) fn trial_intent(&mut self, id: &str, single: Option<i64>, regime: Regime) -> Seq {
        let intent = TrialIntent {
            trial: TrialId::from(id.to_owned()),
            kernel_cursor: None,
            kernel_error: None,
            profile: PerCore::default(),
            core: single.map(core),
            cores: if single.is_some() {
                CoreSet::default()
            } else {
                [core(0), core(8)].into_iter().collect()
            },
            offset: single.map(|_| target(-10)),
            regime,
            workload: pick_workload(regime, 0).id(),
            duration_s: 60,
            condition: if single.is_some() {
                Condition::Alone
            } else {
                Condition::Together
            },
            activity: Some(Activity::Search),
            retry: false,
            cycle: None,
            step: None,
            hunt: None,
            group: None,
            round: None,
            rerun: false,
        };
        self.add(Payload::TrialIntent(intent), &[])
    }

    pub(crate) fn started(&mut self, id: &str, window_start_ns: Option<u64>, cause: Seq) -> Seq {
        self.add(
            Payload::TrialStart(TrialStart {
                trial: TrialId::from(id.to_owned()),
                window_start_ns,
                scope: "scope".into(),
                pid: 1,
                cpus: vec![CpuId::from(0), CpuId::from(8)],
                argv: Vec::new(),
                files: Vec::new(),
                instances: Vec::new(),
            }),
            &[cause],
        )
    }

    pub(crate) fn ended(&mut self, id: &str, outcome: Outcome, cause: &[Seq]) -> Seq {
        self.add(Payload::TrialEnd(trial_end(id, outcome)), cause)
    }

    fn mce(&mut self, mce: Mce) -> Seq {
        self.add(Payload::Mce(mce), &[])
    }

    pub(crate) fn crashed(&mut self, previous: &str, causes: &[Seq]) -> Seq {
        self.add(Payload::CrashDetected(crash(previous)), causes)
    }

    pub(crate) fn state(&self) -> RecoveryState {
        RecoveryState::replay(&self.events).unwrap()
    }
}

fn trial_end(id: &str, outcome: Outcome) -> TrialEnd {
    crate::evidence::trial_end(TrialId::from(id.to_owned()), outcome, 60)
}

fn crash(previous: &str) -> CrashDetected {
    CrashDetected {
        previous_boot: boot(previous),
        in_flight: None,
        stray: false,
        condition: None,
        reset_reason: None,
        reset_reason_raw: Vec::new(),
        inconclusive: false,
        pstore: None,
    }
}

fn mce(corrected: bool) -> Mce {
    Mce {
        cpu: CpuId::from(0),
        core: Some(core(0)),
        bank: 1,
        bank_type: BankType::LoadStore,
        corrected,
        monotonic_ns: None,
        from_boot: None,
        trial: None,
        between_trials: false,
        lines: vec!["machine check".into()],
    }
}

fn warning(text: &str) -> Payload {
    Payload::SessionWarning(SessionWarning {
        operation: text.into(),
        trial: None,
        error: String::new(),
    })
}

pub(crate) fn shutdown(reason: ShutdownReason) -> Payload {
    Payload::Shutdown(Shutdown {
        reason,
        kernel_cursor: None,
        kernel_error: None,
        cycles: None,
    })
}

pub(crate) fn dead_end(condition: DeadEndCondition) -> Payload {
    Payload::DeadEnd(DeadEnd {
        condition,
        core: None,
        detail: "detail".into(),
        action: None,
    })
}

// A session that does not hold its own events is no use to a later boot: the kinds that never replace a shutdown.

#[test]
fn a_warning_or_leave_reason_never_replaces_a_shutdown() {
    let mut log = Log::new();
    log.add(shutdown(ShutdownReason::Signal), &[]);
    log.add(warning("late"), &[]);
    log.add(
        Payload::BootLeaveReason(BootLeaveReason {
            reason_id: "id".into(),
            restart_limit_count: 1,
            reason: "r".into(),
        }),
        &[],
    );
    let state = log.state();
    assert_eq!(state.last_kind(&boot("b0")), Some(EventKind::Shutdown));
    assert_eq!(state.crashed_boots(&boot("b1")).count(), 0);
}

#[test]
fn a_boot_without_a_shutdown_crashed_unless_it_is_current_or_a_crash_was_recorded() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.at("b1", 0).add(warning("first of the boot"), &[]);
    let state = log.state();
    assert_eq!(state.boots(), [boot("b0"), boot("b1")]);
    assert_eq!(
        state.crashed_boots(&boot("b1")).collect::<Vec<_>>(),
        [&boot("b0")]
    );
    assert_eq!(
        state.crashed_boots(&boot("b0")).collect::<Vec<_>>(),
        [&boot("b1")]
    );
    log.crashed("b0", &[]);
    assert_eq!(log.state().crashed_boots(&boot("b1")).count(), 0);
    assert_eq!(log.state().crash_of(&boot("b0")), Some(log.next().prev()));
}

trait Prev {
    fn prev(self) -> Seq;
}

impl Prev for Seq {
    fn prev(self) -> Seq {
        Seq::new(self.get() - 1).unwrap()
    }
}

#[test]
fn the_latest_unanswered_intent_of_a_boot_is_found_not_the_last_event() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let trial = log.trial_intent("0001", Some(0), Regime::R1);
    let smu = log.intent(0, -10, &[trial]);
    log.add(warning("noise"), &[]);
    let state = log.state();
    assert_eq!(state.latest_unmatched_intent(&boot("b0")), Some(smu));
    assert_eq!(state.latest_unmatched_intent(&boot("b1")), None);
    let kinds: Vec<_> = state
        .unmatched_intents()
        .iter()
        .map(|open| open.kind.clone())
        .collect();
    assert_eq!(
        kinds,
        [
            IntentKind::Trial(TrialId::from("0001".to_owned())),
            IntentKind::Smu
        ]
    );

    let write = log.written(0, -10, smu);
    assert_eq!(
        log.state().latest_unmatched_intent(&boot("b0")),
        Some(trial)
    );
    log.readback(0, -10, -10, write);
    log.ended("0001", Outcome::Pass, &[]);
    assert_eq!(log.state().latest_unmatched_intent(&boot("b0")), None);
}

#[test]
fn a_new_smu_intent_replaces_one_nothing_answered_and_a_crash_clears_its_boots() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.intent(0, -10, &[]);
    let second = log.intent(8, -5, &[]);
    assert_eq!(log.state().unmatched_intents().len(), 1);
    assert_eq!(
        log.state().latest_unmatched_intent(&boot("b0")),
        Some(second)
    );
    log.at("b1", 0).crashed("b0", &[]);
    assert_eq!(log.state().latest_unmatched_intent(&boot("b0")), None);
}

#[test]
fn registers_are_the_baseline_lowered_by_intents_and_certain_after_a_readback_by_core_id() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, -3)]);
    let intent = log.intent(8, -12, &[]);
    let state = log.state();
    let registers = state.registers(&boot("b0")).unwrap();
    assert_eq!(registers.offset(core(8)), Some(ObservedOffset(-12)));
    assert!(registers.uncertain(core(8)));
    assert_eq!(registers.offset(core(0)), Some(ObservedOffset(0)));
    assert!(!registers.uncertain(core(0)));
    assert!(!registers.certain());

    let write = log.written(8, -12, intent);
    log.readback(8, -12, -12, write);
    let state = log.state();
    let registers = state.registers(&boot("b0")).unwrap();
    assert!(!registers.uncertain(core(8)));
    assert_eq!(registers.offset(core(8)), Some(ObservedOffset(-12)));
    assert!(registers.certain());
    assert_eq!(
        registers.offsets().iter().collect::<Vec<_>>(),
        [(core(0), ObservedOffset(0)), (core(8), ObservedOffset(-12))]
    );
}

#[test]
fn an_intent_never_raises_the_conservative_register_and_a_set_all_covers_every_core() {
    let mut log = Log::new();
    log.baseline(&[(0, -20), (8, -30)]);
    log.intent_all(0, &[]);
    let state = log.state();
    let registers = state.registers(&boot("b0")).unwrap();
    assert_eq!(registers.offset(core(0)), Some(ObservedOffset(-20)));
    assert_eq!(registers.offset(core(8)), Some(ObservedOffset(-30)));
    assert!(registers.uncertain(core(0)) && registers.uncertain(core(8)));
}

#[test]
fn a_boot_that_wrote_before_a_baseline_starts_from_nothing_known() {
    let mut log = Log::new();
    log.intent(0, -4, &[]);
    let state = log.state();
    let registers = state.registers(&boot("b0")).unwrap();
    assert_eq!(registers.offset(core(0)), Some(ObservedOffset(-4)));
    assert_eq!(registers.offset(core(8)), None);
}

#[test]
fn a_nonzero_write_outside_a_trial_is_an_application_together_or_parked_once_a_group_arms_it() {
    let mut log = Log::new();
    let baseline = log.baseline(&[(0, 0), (8, 0)]);
    log.intent(0, -10, &[baseline]);
    assert_eq!(
        log.state().applied(&boot("b0")),
        None,
        "baseline reads are not applications"
    );
    log.intent_all(0, &[]);
    assert_eq!(log.state().applied(&boot("b0")), None, "a zero is not");
    let first = log.intent(0, -10, &[]);
    assert_eq!(
        log.state().applied(&boot("b0")),
        Some((first, Condition::Together))
    );

    let mut parked = Log::new();
    parked.baseline(&[(0, 0), (8, 0)]);
    parked.add(hunt_group(), &[]);
    assert!(parked.state().hunt_group_armed());
    let first = parked.intent(0, -10, &[]);
    assert_eq!(
        parked.state().applied(&boot("b0")),
        Some((first, Condition::Parked))
    );
}

#[test]
fn a_trial_in_flight_makes_no_application() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.trial_intent("0001", Some(0), Regime::R1);
    log.intent(0, -10, &[]);
    assert_eq!(log.state().applied(&boot("b0")), None);
}

pub(crate) fn hunt_group() -> Payload {
    Payload::HuntGroup(HuntGroup {
        hunt: std::num::NonZeroU32::MIN,
        group: std::num::NonZeroU32::MIN,
        cores: CoreSet::default(),
        profile: PerCore::default(),
        set: CoreSet::default(),
        granularity: 2,
        stage: HuntStage::Part,
        index: 0,
        duration_s: 60,
        escalated: false,
        full_checked: false,
        any_failed: false,
        probe: None,
        held: Vec::new(),
        inferred: None,
        skipped: false,
        reason: None,
    })
}

fn applied(condition: Condition) -> Payload {
    Payload::ProfileApplied(ProfileApplied {
        offsets: PerCore::default(),
        condition,
    })
}

#[test]
fn a_hunt_group_stays_armed_through_a_restoration_until_something_replaces_it() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.add(hunt_group(), &[]);
    log.add(applied(Condition::Parked), &[]);
    assert!(log.state().hunt_group_armed());
    log.add(
        Payload::ProfileRestored(ProfileRestored {
            offsets: PerCore::default(),
        }),
        &[],
    );
    assert!(log.state().hunt_group_armed());
    assert_eq!(log.state().applied(&boot("b0")), None);
    let again = log.intent(0, -10, &[]);
    assert_eq!(
        log.state().applied(&boot("b0")),
        Some((again, Condition::Parked))
    );
}

#[test]
fn a_together_application_a_profile_change_or_the_hunts_end_disarms_the_group() {
    let disarming: [Payload; 3] = [
        applied(Condition::Together),
        Payload::ProfileChange(ProfileChange {
            from: None,
            to: PerCore::default(),
        }),
        Payload::HuntEnd(HuntEnd {
            hunt: std::num::NonZeroU32::MIN,
            result: HuntOutcome::Fallback,
            cores: CoreSet::default(),
            members: Vec::new(),
            groups: 1,
            reason: "r".into(),
        }),
    ];
    for payload in disarming {
        let mut log = Log::new();
        log.baseline(&[(0, 0), (8, 0)]);
        log.add(hunt_group(), &[]);
        log.add(payload.clone(), &[]);
        assert!(!log.state().hunt_group_armed(), "{:?}", payload.kind());
    }
    let mut log = Log::new();
    log.add(hunt_group(), &[]);
    log.trial_intent("0001", Some(0), Regime::R1);
    assert!(!log.state().hunt_group_armed(), "a together trial intent");
}

#[test]
fn idle_evidence_starts_at_the_applications_first_write() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.mono(1000).intent(0, -10, &[]);
    log.mono(2000).intent(8, -5, &[]);
    assert_eq!(
        log.state().applied_mono(&boot("b0")),
        Some(Duration::from_secs(1))
    );
    assert!(log.state().applying(&boot("b0")));
    log.mono(3000).add(applied(Condition::Together), &[]);
    assert_eq!(
        log.state().applied_mono(&boot("b0")),
        Some(Duration::from_secs(1))
    );
    assert!(!log.state().applying(&boot("b0")));
    log.mono(4000).intent(0, -11, &[]);
    assert_eq!(
        log.state().applied_mono(&boot("b0")),
        Some(Duration::from_secs(4))
    );
}

#[test]
fn an_open_trial_ran_from_its_start_to_its_last_record_by_the_monotonic_clock_whatever_the_wall_did()
 {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.mono(10_000).wall(0);
    let intent = log.trial_intent("0001", None, Regime::R7);
    let start = log
        .mono(10_000)
        .started("0001", Some(10_000_000_000), intent);
    assert_eq!(log.state().open_trial().unwrap().ran(), Duration::ZERO);
    log.mono(95_000).wall(-3_600);
    log.add(
        Payload::TrialProgress(TrialProgress {
            trial: TrialId::from("0001".to_owned()),
            detail: "running".into(),
            signal: None,
            core: None,
        }),
        &[start],
    );
    assert_eq!(
        log.state().open_trial().unwrap().ran(),
        Duration::from_secs(85)
    );
}

#[test]
fn a_sample_of_the_open_trial_is_its_last_activity_and_another_trials_sample_is_not() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let intent = log.mono(10_000).trial_intent("0001", None, Regime::R7);
    let start = log
        .mono(10_000)
        .started("0001", Some(10_000_000_000), intent);
    let sample = |trial: &str| {
        Payload::TrialSample(togi_journal::TrialSample {
            trial: TrialId::from(trial.to_owned()),
            warning: "sample".into(),
            pid: 1,
            tid: 1,
            cpu: togi_machine::CpuId::from(0),
        })
    };
    log.mono(40_000).add(sample("0001"), &[start]);
    assert_eq!(
        log.state().open_trial().unwrap().ran(),
        Duration::from_secs(30)
    );
    log.mono(90_000).add(sample("0002"), &[start]);
    assert_eq!(
        log.state().open_trial().unwrap().ran(),
        Duration::from_secs(30)
    );
}

#[test]
fn without_monotonic_times_the_wall_clock_gives_the_duration() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.wall(0);
    let intent = log.trial_intent("0001", None, Regime::R7);
    log.at("b0", 0);
    log.mono_ms = None;
    let start = log.wall(10).started("0001", None, intent);
    log.wall(70).add(
        Payload::TrialSignal(togi_journal::TrialSignal {
            trial: TrialId::from("0001".to_owned()),
            schedule: None,
            stops: 0,
            conts: 0,
        }),
        &[start],
    );
    assert_eq!(
        log.state().open_trial().unwrap().ran(),
        Duration::from_secs(60)
    );
}

#[test]
fn a_progress_record_of_another_trial_is_not_this_trials_activity_or_failure() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let intent = log.mono(10_000).trial_intent("0001", None, Regime::R7);
    log.started("0001", None, intent);
    log.mono(50_000).add(
        Payload::TrialProgress(TrialProgress {
            trial: TrialId::from("0099".to_owned()),
            detail: "stray".into(),
            signal: Some(Signal::Stall),
            core: Some(core(0)),
        }),
        &[],
    );
    let state = log.state();
    let open = state.open_trial().unwrap();
    assert_eq!(open.failure, None);
    assert_eq!(open.ran(), Duration::ZERO);
}

#[test]
fn a_backend_signal_is_kept_on_the_open_trial() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let intent = log.trial_intent("0001", Some(0), Regime::R1);
    log.started("0001", None, intent);
    log.add(
        Payload::TrialProgress(TrialProgress {
            trial: TrialId::from("0001".to_owned()),
            detail: "core 00 stalled".into(),
            signal: Some(Signal::Stall),
            core: Some(core(0)),
        }),
        &[],
    );
    assert_eq!(
        log.state().open_trial().unwrap().failure,
        Some((Signal::Stall, Some(core(0))))
    );
}

fn live_trial(cores: &mut Log) {
    cores.baseline(&[(0, 0), (8, 0)]);
    cores.at("old", 10_000);
    let intent = cores.trial_intent("window", None, Regime::R7);
    cores
        .mono(11_000)
        .started("window", Some(10_000_000_000), intent);
}
struct Case {
    name: &'static str,
    from_boot: Option<&'static str>,
    event_boot: &'static str,
    mono_ns: Option<u64>,
    corrected: bool,
    between: bool,
    want: bool,
}

#[test]
fn which_machine_checks_belong_to_the_trial_and_which_to_its_crash() {
    let ns = |n: u64| Some(n);
    let ten = 10_000_000_000_u64;
    let case = |name, from_boot, event_boot, mono_ns, corrected, between, want| Case {
        name,
        from_boot,
        event_boot,
        mono_ns,
        corrected,
        between,
        want,
    };
    let cases = [
        case(
            "recovered before the window",
            Some("old"),
            "new",
            ns(ten - 1),
            false,
            false,
            false,
        ),
        case(
            "recovered at the window start",
            Some("old"),
            "new",
            ns(ten),
            false,
            false,
            true,
        ),
        case(
            "recovered unread crash tail",
            Some("old"),
            "new",
            ns(12_000_000_000),
            false,
            false,
            true,
        ),
        case(
            "recovered zero is a time",
            Some("old"),
            "new",
            ns(0),
            false,
            false,
            false,
        ),
        case(
            "retained by the next boot, uncorrected, at zero",
            Some("new"),
            "new",
            ns(0),
            false,
            false,
            true,
        ),
        case(
            "retained by the next boot, uncorrected, late",
            Some("new"),
            "new",
            ns(3_600_000_000_000),
            false,
            false,
            true,
        ),
        case(
            "retained by the next boot, corrected",
            Some("new"),
            "new",
            ns(0),
            true,
            false,
            false,
        ),
        case(
            "another boot",
            Some("other"),
            "new",
            ns(12_000_000_000),
            false,
            false,
            false,
        ),
        case(
            "live before the window",
            None,
            "old",
            ns(0),
            false,
            false,
            false,
        ),
        case(
            "live at the window start",
            None,
            "old",
            ns(ten),
            false,
            false,
            true,
        ),
        case(
            "live from a different boot",
            None,
            "new",
            ns(12_000_000_000),
            false,
            false,
            false,
        ),
        case(
            "live after the window",
            None,
            "old",
            ns(12_000_000_000),
            false,
            true,
            false,
        ),
        case("live without a time", None, "old", None, false, false, true),
    ];
    for case in cases {
        let mut log = Log::new();
        live_trial(&mut log);
        log.at(case.event_boot, 12_000);
        log.mce(Mce {
            from_boot: case.from_boot.map(boot),
            monotonic_ns: case.mono_ns,
            corrected: case.corrected,
            between_trials: case.between,
            ..mce(false)
        });
        let state = log.state();
        let mut seqs = state.open_trial().unwrap().mces.clone();
        seqs.extend(state.recorded_for(&boot("old"), &boot("new")));
        let want: Vec<Seq> = if case.want {
            vec![log.next().prev()]
        } else {
            Vec::new()
        };
        assert_eq!(seqs, want, "{}", case.name);
    }
}

#[test]
fn a_crash_claims_its_machine_checks_once() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.at("a", 5_000);
    let tail = log.at("b", 0).mce(Mce {
        from_boot: Some(boot("a")),
        monotonic_ns: Some(1_000),
        ..mce(false)
    });
    assert_eq!(log.state().recorded_for(&boot("a"), &boot("c")), [tail]);
    assert_eq!(
        log.state().recorded_for(&boot("b"), &boot("c")),
        Vec::<Seq>::new()
    );
    log.at("c", 0).crashed("a", &[tail]);
    let state = log.state();
    assert_eq!(state.recorded_for(&boot("a"), &boot("c")), [tail]);
    assert_eq!(
        state.recorded_for(&boot("z"), &boot("c")),
        Vec::<Seq>::new()
    );
    let mut stolen = Log::new();
    stolen.baseline(&[(0, 0), (8, 0)]);
    let tail = stolen.at("a", 5_000).at("b", 0).mce(Mce {
        from_boot: Some(boot("a")),
        monotonic_ns: Some(1_000),
        ..mce(false)
    });
    stolen.at("c", 0).crashed("a", &[tail]);
    assert_eq!(
        stolen.state().recorded_for(&boot("b"), &boot("c")),
        Vec::<Seq>::new()
    );
}

#[test]
fn a_machine_check_is_the_same_by_boot_time_cpu_and_bank_not_by_its_text() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.at("b0", 4_000).mce(Mce {
        monotonic_ns: Some(4_000_000_000),
        ..mce(true)
    });
    let state = log.state();
    let seen = togi_machine::Mce {
        cpu: CpuId::from(0),
        core: Some(core(0)),
        bank: 1,
        bank_type: BankType::LoadStore,
        corrected: true,
        time: Timestamp::UNIX_EPOCH,
        monotonic: Duration::from_secs(4),
        lines: vec!["different text".into()],
    };
    assert!(state.has_recorded(&boot("b0"), &seen));
    assert!(!state.has_recorded(&boot("b1"), &seen));
    for different in [
        togi_machine::Mce {
            monotonic: Duration::from_secs(5),
            ..seen.clone()
        },
        togi_machine::Mce {
            cpu: CpuId::from(1),
            ..seen.clone()
        },
        togi_machine::Mce {
            bank: 2,
            ..seen.clone()
        },
    ] {
        assert!(!state.has_recorded(&boot("b0"), &different));
    }
}

#[test]
fn a_kernel_read_error_is_sticky_on_its_open_trial_and_cursors_are_per_boot() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let cursor = |text: &str| Some(KernelCursor::from(text.to_owned()));
    let mut intent = trial_intent_payload();
    intent.kernel_cursor = cursor("c1");
    intent.kernel_error = Some("kernel log unreadable: first".into());
    log.add(Payload::TrialIntent(intent), &[]);
    assert_eq!(
        log.state().kernel_cursor(&boot("b0")),
        cursor("c1").as_ref()
    );
    let mut end = trial_end("0001", Outcome::Inconclusive);
    end.kernel_cursor = cursor("c2");
    end.kernel_error = Some("second".into());
    log.add(Payload::TrialEnd(end), &[]);
    assert_eq!(
        log.state().kernel_cursor(&boot("b0")),
        cursor("c2").as_ref()
    );
    assert_eq!(log.state().kernel_cursor(&boot("b1")), None);

    let mut open = Log::new();
    open.baseline(&[(0, 0), (8, 0)]);
    let mut intent = trial_intent_payload();
    intent.kernel_error = Some("first".into());
    open.add(Payload::TrialIntent(intent), &[]);
    open.add(
        Payload::ConfigLoaded(config_loaded(None, Some("second"))),
        &[],
    );
    assert_eq!(
        open.state().open_trial().unwrap().kernel_error.as_deref(),
        Some("first; second")
    );
    open.at("b1", 0).add(
        Payload::ConfigLoaded(config_loaded(None, Some("elsewhere"))),
        &[],
    );
    assert_eq!(
        open.state().open_trial().unwrap().kernel_error.as_deref(),
        Some("first; second")
    );
}

fn trial_intent_payload() -> TrialIntent {
    let mut log = Log::new();
    log.trial_intent("0001", None, Regime::R7);
    let payload = log.events.pop().unwrap().payload;
    let Payload::TrialIntent(intent) = payload else {
        panic!("{payload:?}");
    };
    intent
}

fn config_loaded(cursor: Option<&str>, error: Option<&str>) -> togi_journal::ConfigLoaded {
    togi_journal::ConfigLoaded {
        build: Build {
            version: "9.9.9".into(),
            rev: Some("abc".into()),
            fixes: FixStamp::through(DefectId::new(4).unwrap()),
            ..build()
        },
        config: togi_config::Config::parse("{}").unwrap().snapshot(),
        path: String::new(),
        file: false,
        kernel_cursor: cursor.map(|text| KernelCursor::from(text.to_owned())),
        kernel_error: error.map(str::to_owned),
    }
}

#[test]
fn config_loaded_updates_the_build_the_run_is_judged_by() {
    let mut log = Log::new();
    log.add(Payload::ConfigLoaded(config_loaded(None, None)), &[]);
    let state = log.state();
    let build = state.build().unwrap();
    assert_eq!(
        (
            build.version.as_str(),
            build.rev.as_deref(),
            build.fixes.get(),
            build.ruleset,
            build.schema
        ),
        ("9.9.9", Some("abc"), 4, 1, SCHEMA)
    );
}

#[test]
fn trial_ids_and_workload_rotation_advance_on_conclusive_trials_only() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let state = log.state();
    assert_eq!(state.next_trial_id().as_str(), "0001");
    assert_eq!(state.rotation(Some(core(0)), Regime::R1), 0);

    log.trial_intent("0001", Some(0), Regime::R1);
    assert_eq!(log.state().next_trial_id().as_str(), "0002");
    log.ended("0001", Outcome::Inconclusive, &[]);
    assert_eq!(log.state().rotation(Some(core(0)), Regime::R1), 0);
    log.trial_intent("0002", Some(0), Regime::R1);
    log.ended("0002", Outcome::Pass, &[]);
    log.trial_intent("0003", Some(0), Regime::R1);
    log.ended("0003", Outcome::Failure, &[]);
    log.trial_intent("0004", None, Regime::R7);
    log.ended("0004", Outcome::Pass, &[]);
    let state = log.state();
    assert_eq!(state.rotation(Some(core(0)), Regime::R1), 2);
    assert_eq!(state.rotation(Some(core(8)), Regime::R1), 0);
    assert_eq!(state.rotation(None, Regime::R7), 1);
    assert_eq!(state.rotation(None, Regime::R1), 0);
    assert_eq!(state.next_trial_id().as_str(), "0005");
}

#[test]
fn inconclusive_trials_streak_unless_interrupted_and_a_conclusive_one_ends_it() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let one = {
        log.trial_intent("0001", Some(0), Regime::R1);
        let mut end = trial_end("0001", Outcome::Inconclusive);
        end.reason = Some("setup failed".into());
        log.add(Payload::TrialEnd(end), &[])
    };
    log.trial_intent("0002", Some(0), Regime::R1);
    let mut interrupted = trial_end("0002", Outcome::Inconclusive);
    interrupted.interrupted = true;
    log.add(Payload::TrialEnd(interrupted), &[]);
    assert_eq!(log.state().streak(Backend::Mprime), [one]);
    assert_eq!(
        log.state().last_inconclusive_reason(Backend::Mprime),
        Some("setup failed")
    );
    assert_eq!(log.state().streak(Backend::Ycruncher), Vec::<Seq>::new());
    log.trial_intent("0003", Some(0), Regime::R1);
    log.ended("0003", Outcome::Pass, &[]);
    assert!(log.state().streak(Backend::Mprime).is_empty());
}

#[test]
fn a_retry_is_recorded_once_per_attempt_and_again_after_a_trial_followed_it() {
    let k = togi_config::DeadEndThreshold::new(1).unwrap();
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.trial_intent("0001", Some(0), Regime::R1);
    log.ended("0001", Outcome::Inconclusive, &[]);
    let due = log.state().backend_retry_due(Backend::Mprime, k).unwrap();
    assert_eq!(
        (due.attempt, due.wait, due.record),
        (1, Duration::from_secs(60), true)
    );
    log.add(
        Payload::BackendRetry(BackendRetry {
            backend: RetryTarget::Mprime,
            attempt: 1,
            wait_s: 60,
            reason: "r".into(),
        }),
        &[],
    );
    let due = log.state().backend_retry_due(Backend::Mprime, k).unwrap();
    assert!(!due.record, "the same durable attempt resumes its wait");
    assert_eq!(log.state().backend_retry_due(Backend::Ycruncher, k), None);
    log.trial_intent("0002", Some(0), Regime::R1);
    log.ended("0002", Outcome::Inconclusive, &[]);
    let due = log.state().backend_retry_due(Backend::Mprime, k).unwrap();
    assert_eq!((due.attempt, due.record), (2, true));
}

#[test]
fn a_backend_inconclusive_through_every_retry_is_exhausted_until_a_no_evidence_dead_end() {
    let k = togi_config::DeadEndThreshold::new(1).unwrap();
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    for n in 1..=4 {
        let id = format!("{n:04}");
        log.trial_intent(&id, Some(0), Regime::R1);
        log.ended(&id, Outcome::Inconclusive, &[]);
        let exhausted = log
            .state()
            .exhausted_backend(k)
            .map(|(backend, streak)| (backend, streak.len()));
        assert_eq!(
            exhausted,
            (n == 4).then_some((Backend::Mprime, 4)),
            "after {n}"
        );
    }
    log.add(dead_end(DeadEndCondition::NoEvidence), &[]);
    assert!(log.state().exhausted_backend(k).is_none());
}

#[test]
fn a_missing_backend_is_evidence_until_the_no_evidence_dead_end() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.trial_intent("0001", Some(0), Regime::R1);
    let mut end = trial_end("0001", Outcome::Inconclusive);
    end.backend_missing = true;
    end.reason = Some("no such file".into());
    let ended = log.add(Payload::TrialEnd(end), &[]);
    let missing = log.state().missing_evidence().cloned().unwrap();
    assert_eq!(missing.seq, ended);
    assert_eq!(missing.detail, "backend mprime is missing: no such file");
    log.add(dead_end(DeadEndCondition::NoEvidence), &[]);
    assert!(log.state().missing_evidence().is_none());
}

#[test]
fn smu_evidence_comes_from_a_failed_command_or_a_disagreeing_readback_until_the_smu_dead_end() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let intent = log.intent(0, -10, &[]);
    let error = log.add(
        Payload::SmuError(SmuError {
            op: SmuOp::Set,
            core: Some(core(0)),
            offset: Some(target(-10)),
            error: "mailbox timeout".into(),
        }),
        &[intent],
    );
    let cited = log.state().smu_evidence().cloned().unwrap();
    assert_eq!(
        (cited.seq, cited.detail.as_str()),
        (error, "SMU command failed: mailbox timeout")
    );
    log.add(dead_end(DeadEndCondition::Smu), &[]);
    assert!(log.state().smu_evidence().is_none());

    let intent = log.intent(0, -10, &[]);
    let write = log.written(0, -10, intent);
    let readback = log.readback(0, -11, -10, write);
    let cited = log.state().smu_evidence().cloned().unwrap();
    assert_eq!(cited.seq, readback);
    assert_eq!(cited.detail, "core 00 reads CO -11 after writing -10");
    let ok = log.readback(0, -10, -10, write);
    assert_eq!(
        log.state().smu_evidence().unwrap().seq,
        readback,
        "{ok}: a later good readback keeps the evidence"
    );
}

#[test]
fn containment_evidence_names_the_escaped_cpus_or_carries_the_error_until_its_dead_end() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let intent = log.trial_intent("0001", None, Regime::R7);
    log.started("0001", None, intent);
    let mut end = trial_end("0001", Outcome::Inconclusive);
    end.escaped = vec![CpuId::from(99), CpuId::from(98)];
    log.add(Payload::TrialEnd(end), &[]);
    assert_eq!(
        log.state().containment_evidence().unwrap().detail,
        "backend thread on cpu 99,98 outside allowed cpus 0,8"
    );
    let intent = log.trial_intent("0002", None, Regime::R7);
    log.started("0002", None, intent);
    let mut end = trial_end("0002", Outcome::Inconclusive);
    end.containment_error = Some("backend still running after teardown".into());
    log.add(Payload::TrialEnd(end), &[]);
    assert_eq!(
        log.state().containment_evidence().unwrap().detail,
        "backend still running after teardown"
    );
    log.add(dead_end(DeadEndCondition::Containment), &[]);
    assert!(log.state().containment_evidence().is_none());
}

fn crash_with(log: &mut Log, previous: &str, edit: impl FnOnce(&mut CrashDetected)) -> Seq {
    let mut detected = crash(previous);
    edit(&mut detected);
    log.add(Payload::CrashDetected(detected), &[])
}

#[test]
fn recorded_crashes_stand_as_stray_idle_or_thermal_evidence_and_nothing_else() {
    struct Case {
        stray: bool,
        inconclusive: bool,
        reset: Option<ResetKind>,
        want: (usize, usize, bool),
    }
    let cases = [
        Case {
            stray: true,
            inconclusive: false,
            reset: None,
            want: (1, 0, false),
        },
        Case {
            stray: false,
            inconclusive: true,
            reset: Some(ResetKind::ThermalTrip),
            want: (0, 0, true),
        },
        Case {
            stray: false,
            inconclusive: true,
            reset: Some(ResetKind::PowerButton),
            want: (0, 0, false),
        },
        Case {
            stray: false,
            inconclusive: true,
            reset: None,
            want: (0, 0, false),
        },
        Case {
            stray: false,
            inconclusive: false,
            reset: None,
            want: (0, 1, false),
        },
    ];
    for case in cases {
        let mut log = Log::new();
        log.baseline(&[(0, 0), (8, 0)]);
        crash_with(&mut log, "a", |detected| {
            detected.stray = case.stray;
            detected.inconclusive = case.inconclusive;
            detected.reset_reason = case.reset;
        });
        let state = log.state();
        assert_eq!(
            (
                state.stray().len(),
                state.pending_idle().len(),
                state.thermal_evidence().is_some()
            ),
            case.want
        );
    }
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.at("a", 0).trial_intent("t1", None, Regime::R1);
    log.at("b", 0);
    crash_with(&mut log, "a", |_| {});
    let state = log.state();
    assert!(state.stray().is_empty() && state.pending_idle().is_empty());
}

#[test]
fn stray_crashes_and_idle_crashes_stand_until_something_answers_them() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let stray = crash_with(&mut log, "x", |detected| detected.stray = true);
    let idle = crash_with(&mut log, "y", |_| {});
    let state = log.state();
    assert_eq!(
        (state.stray(), state.pending_idle()),
        (&[stray][..], &[idle][..])
    );
    let idle_crashes: Vec<_> = state.idle_crashes().collect();
    assert_eq!(idle_crashes, [(idle, &crash("y"))]);
    assert_eq!(state.crash(&boot("x")).map(|(seq, _)| seq), Some(stray));
    assert!(
        state
            .crash(&boot("x"))
            .is_some_and(|(_, detected)| detected.stray)
    );
    log.add(
        Payload::Failure(togi_journal::Failure {
            signal: Signal::Crash,
            attribution: togi_journal::Attribution::Unattributed,
            core: None,
            offset: None,
            trial: None,
            regime: None,
            condition: None,
            profile: None,
            known_failure: None,
            reason: None,
            round: None,
        }),
        &[idle],
    );
    assert!(log.state().pending_idle().is_empty());
    assert_eq!(log.state().idle_crashes().count(), 0);
    log.add(dead_end(DeadEndCondition::BootLoop), &[]);
    assert!(log.state().stray().is_empty());
    log.intent(0, -10, &[]);
    let stray = crash_with(&mut log, "z", |detected| detected.stray = true);
    assert_eq!(log.state().stray(), [stray]);
    log.add(applied(Condition::Together), &[]);
    assert!(
        log.state().stray().is_empty(),
        "an application means the next crash is not stray"
    );
}

#[test]
fn a_thermal_trip_stands_as_evidence_until_its_dead_end_and_kernel_retries_end_with_a_crash() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    log.add(
        Payload::BackendRetry(BackendRetry {
            backend: RetryTarget::KernelLog,
            attempt: 1,
            wait_s: 60,
            reason: "kernel log".into(),
        }),
        &[],
    );
    assert_eq!(log.state().kernel_retries().len(), 1);
    let thermal = crash_with(&mut log, "x", |detected| {
        detected.inconclusive = true;
        detected.reset_reason = Some(ResetKind::ThermalTrip);
        detected.reset_reason_raw = vec!["thermal trip".into()];
    });
    let state = log.state();
    assert!(state.kernel_retries().is_empty());
    let cited = state.thermal_evidence().unwrap();
    assert_eq!(cited.seq, thermal);
    assert_eq!(
        cited.detail,
        "the machine reset on a thermal trip (thermal trip); check cooling before tuning again"
    );
    log.add(dead_end(DeadEndCondition::ThermalTrip), &[]);
    assert!(log.state().thermal_evidence().is_none());
}

#[test]
fn kernel_retries_end_with_a_no_evidence_dead_end_so_a_later_run_retries_afresh() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let retry = log.add(
        Payload::BackendRetry(BackendRetry {
            backend: RetryTarget::KernelLog,
            attempt: 1,
            wait_s: 60,
            reason: "kernel log".into(),
        }),
        &[],
    );
    assert_eq!(log.state().kernel_retries(), [retry]);
    log.add(dead_end(DeadEndCondition::Smu), &[]);
    assert_eq!(log.state().kernel_retries(), [retry]);
    log.add(dead_end(DeadEndCondition::NoEvidence), &[]);
    assert!(log.state().kernel_retries().is_empty());
}

#[test]
fn a_dead_end_is_pending_until_its_saved_entry_is_cleared_and_its_shutdown_recorded() {
    let mut log = Log::new();
    let evidence = log.baseline(&[(0, 0), (8, 0)]);
    assert!(log.state().pending_dead_end().is_none());
    let dead = log.add(dead_end(DeadEndCondition::Preflight), &[evidence]);
    let pending = log.state().pending_dead_end().cloned().unwrap();
    assert_eq!(pending.seq, dead);
    assert_eq!(pending.causes, [evidence]);
    assert_eq!(pending.saved_entry_cleared(), None);
    assert_eq!(pending.dead_end.condition, DeadEndCondition::Preflight);
    assert!(pending.needs_saved_entry());

    let entry = |cause: &[Seq], log: &mut Log| {
        log.add(
            Payload::BootSavedEntry(BootSavedEntry {
                before: None,
                after: None,
                error: None,
                reason_error: None,
            }),
            cause,
        )
    };
    entry(&[], &mut log);
    assert!(
        log.state().pending_dead_end().unwrap().needs_saved_entry(),
        "another entry is not its entry"
    );
    entry(&[dead], &mut log);
    assert!(!log.state().pending_dead_end().unwrap().needs_saved_entry());
    assert_eq!(
        log.state()
            .pending_dead_end()
            .unwrap()
            .saved_entry_cleared(),
        Some(true)
    );
    log.add(
        Payload::BootSavedEntry(BootSavedEntry {
            before: None,
            after: None,
            error: Some("grub-editenv failed".into()),
            reason_error: None,
        }),
        &[dead],
    );
    assert_eq!(
        log.state()
            .pending_dead_end()
            .unwrap()
            .saved_entry_cleared(),
        Some(false),
        "the latest attempt counts"
    );
    log.add(shutdown(ShutdownReason::Signal), &[]);
    assert!(
        log.state().pending_dead_end().is_some(),
        "a clean stop is not its shutdown"
    );
    log.add(shutdown(ShutdownReason::DeadEnd), &[]);
    assert!(log.state().pending_dead_end().is_none());
    let newer = log.add(dead_end(DeadEndCondition::Smu), &[]);
    assert_eq!(log.state().pending_dead_end().unwrap().seq, newer);
}

#[test]
fn baseline_phases_notices_and_carry_are_kept_for_startup() {
    let mut log = Log::new();
    let baseline = log.baseline(&[(0, -5), (8, 0)]);
    log.add(
        Payload::CorePhase(CoreTransition {
            core: core(8),
            from: None,
            to: CorePhase::AtLimit,
            offset: target(-3),
            pass: None,
            failure_point: None,
            check_solo_limit: false,
            workloads: Vec::new(),
            cleared_combination: Vec::new(),
            reason: "r".into(),
        }),
        &[],
    );
    let carried = log.add(
        Payload::SessionCarried(SessionCarried {
            sources: Vec::new(),
            failure_points: false,
            detail: None,
            carried: vec![CarriedCore {
                core: core(8),
                candidate_solo_limit: None,
                failure_point: None,
            }],
        }),
        &[],
    );
    let state = log.state();
    let (seq, offsets) = state.baseline().unwrap();
    assert_eq!(
        (seq, offsets.get(core(0))),
        (baseline, Some(ObservedOffset(-5)))
    );
    assert_eq!(state.phase(core(8)), Some(CorePhase::AtLimit));
    assert_eq!(state.phase(core(0)), None);
    assert_eq!(
        state.carried().map(|(seq, cores)| (seq, cores.len())),
        Some((carried, 1))
    );
    assert_eq!(state.topology().len(), 2);
    assert_eq!(
        state.topology().get(&core(8)),
        Some(&CcdId::new(1).unwrap())
    );
    assert!(!state.noticed());
}

#[test]
fn an_event_the_state_cannot_take_in_leaves_it_unchanged() {
    let mut log = Log::new();
    log.baseline(&[(0, 0), (8, 0)]);
    let before = log.state();
    let mut state = before.clone();
    let second = Event::new(
        log.next(),
        log.stamp(),
        boot("b0"),
        Vec::new(),
        log.events[0].payload.clone(),
    );
    assert!(matches!(
        state.apply(&second),
        Err(FoldError::DuplicateStart { seq }) if seq == second.seq
    ));
    assert_eq!(state, before);

    let mut empty = RecoveryState::default();
    let warn = Event::new(
        seq(1),
        log.stamp(),
        boot("b0"),
        Vec::new(),
        warning("early"),
    );
    assert!(matches!(
        empty.apply(&warn),
        Err(FoldError::BeforeStart {
            seq: at,
            kind: EventKind::SessionWarning
        }) if at == seq(1)
    ));
    assert_eq!(empty, RecoveryState::default());
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Intent(u8, u8),
    IntentAll,
    Write,
    Readback(u8, u8, u8),
    SmuError,
    TrialIntent(bool),
    TrialStart,
    Progress,
    TrialEnd(u8),
    Mce(bool, bool),
    Crash(bool, bool),
    Applied(bool),
    Restored,
    Warning,
    Shutdown,
    DeadEnd(u8),
    Retry(u8),
    Group,
    Next(u8),
}

fn steps() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0..2_u8, 0..4_u8).prop_map(|(c, o)| Step::Intent(c, o)),
        Just(Step::IntentAll),
        Just(Step::Write),
        (0..2_u8, 0..4_u8, 0..4_u8).prop_map(|(c, a, b)| Step::Readback(c, a, b)),
        Just(Step::SmuError),
        any::<bool>().prop_map(Step::TrialIntent),
        Just(Step::TrialStart),
        Just(Step::Progress),
        (0..4_u8).prop_map(Step::TrialEnd),
        (any::<bool>(), any::<bool>()).prop_map(|(a, b)| Step::Mce(a, b)),
        (any::<bool>(), any::<bool>()).prop_map(|(a, b)| Step::Crash(a, b)),
        any::<bool>().prop_map(Step::Applied),
        Just(Step::Restored),
        Just(Step::Warning),
        Just(Step::Shutdown),
        (0..8_u8).prop_map(Step::DeadEnd),
        (0..3_u8).prop_map(Step::Retry),
        Just(Step::Group),
        (0..3_u8).prop_map(Step::Next),
    ]
}

fn play(log: &mut Log, step: Step) {
    let id = format!("{:04}", log.next().get() % 5);
    let cores = [0, 8];
    let last = log.events.last().map(|event| event.seq).unwrap();
    let (payload, causes) = match step {
        Step::Intent(c, o) => {
            log.intent(cores[usize::from(c)], -i64::from(o) * 7, &[last]);
            return;
        }
        Step::IntentAll => {
            log.intent_all(0, &[]);
            return;
        }
        Step::Write => {
            log.written(0, 0, last);
            return;
        }
        Step::Readback(c, a, b) => {
            log.readback(cores[usize::from(c)], -i64::from(a), -i64::from(b), last);
            return;
        }
        Step::Next(boot_number) => {
            log.at(&format!("b{boot_number}"), u64::from(boot_number) * 500);
            return;
        }
        Step::TrialIntent(single) => {
            log.trial_intent(&id, single.then_some(0), Regime::R1);
            return;
        }
        Step::TrialStart => {
            log.started(&id, None, last);
            return;
        }
        Step::SmuError => (
            Payload::SmuError(SmuError {
                op: SmuOp::Read,
                core: None,
                offset: None,
                error: "e".into(),
            }),
            Vec::new(),
        ),
        Step::Progress => (
            Payload::TrialProgress(TrialProgress {
                trial: TrialId::from(id),
                detail: "p".into(),
                signal: Some(Signal::Stall),
                core: None,
            }),
            Vec::new(),
        ),
        Step::TrialEnd(n) => {
            let outcome = [
                Outcome::Pass,
                Outcome::Failure,
                Outcome::Inconclusive,
                Outcome::Inconclusive,
            ][usize::from(n)];
            let mut end = trial_end(&id, outcome);
            end.interrupted = n == 3;
            end.escaped = if n == 2 {
                vec![CpuId::from(1)]
            } else {
                Vec::new()
            };
            (Payload::TrialEnd(end), Vec::new())
        }
        Step::Mce(corrected, recovered) => (
            Payload::Mce(Mce {
                from_boot: recovered.then(|| boot("b0")),
                monotonic_ns: Some(log.next().get() * 1_000),
                ..mce(corrected)
            }),
            Vec::new(),
        ),
        Step::Crash(stray, inconclusive) => (
            Payload::CrashDetected(CrashDetected {
                stray,
                inconclusive,
                ..crash("b0")
            }),
            vec![last],
        ),
        Step::Applied(parked) => (
            applied(if parked {
                Condition::Parked
            } else {
                Condition::Together
            }),
            Vec::new(),
        ),
        Step::Restored => (
            Payload::ProfileRestored(ProfileRestored {
                offsets: PerCore::default(),
            }),
            Vec::new(),
        ),
        Step::Warning => (warning("w"), Vec::new()),
        Step::Shutdown => (shutdown(ShutdownReason::Signal), Vec::new()),
        Step::DeadEnd(n) => (
            dead_end(DeadEndCondition::ALL[usize::from(n) % DeadEndCondition::ALL.len()]),
            Vec::new(),
        ),
        Step::Retry(n) => (
            Payload::BackendRetry(BackendRetry {
                backend: [
                    RetryTarget::Mprime,
                    RetryTarget::Ycruncher,
                    RetryTarget::KernelLog,
                ][usize::from(n)],
                attempt: 1,
                wait_s: 60,
                reason: "r".into(),
            }),
            Vec::new(),
        ),
        Step::Group => (hunt_group(), Vec::new()),
    };
    log.add(payload, &causes);
}

proptest! {
    #![proptest_config(ProptestConfig {
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x7265_636f_7665_7279),
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn a_fresh_fold_of_every_prefix_is_the_incremental_fold(script in prop::collection::vec(steps(), 0..48)) {
        let mut log = Log::new();
        log.baseline(&[(0, 0), (8, 0)]);
        for step in script {
            play(&mut log, step);
        }
        let mut incremental = RecoveryState::default();
        for (applied, event) in log.events.iter().enumerate() {
            incremental.apply(event).unwrap();
            let fresh = RecoveryState::replay(&log.events[..=applied]).unwrap();
            prop_assert_eq!(&fresh, &incremental);
        }
        let mut resumed = RecoveryState::replay(&log.events[..log.events.len() / 2]).unwrap();
        for event in &log.events[log.events.len() / 2..] {
            resumed.apply(event).unwrap();
        }
        prop_assert_eq!(resumed, incremental);
    }
}

#[test]
fn a_sparse_machine_is_keyed_by_core_id_not_by_position() {
    let log = Log::on(&[(3, 0), (12, 1)]);
    let state = log.state();
    assert_eq!(
        state.topology().keys().copied().collect::<Vec<_>>(),
        [core(3), core(12)]
    );
}

#[test]
fn a_trial_alone_s_window_starts_at_the_readback_of_its_target_and_not_before_the_write() {
    let window = |log: &Log| {
        let state = log.state();
        let open = state.open_trial().unwrap();
        (open.target, open.window_start)
    };
    let mut log = Log::new();
    log.at("b0", 1_000);
    let trial = log.trial_intent("0001", Some(0), Regime::R7);
    assert_eq!(window(&log), (None, Some(Duration::from_secs(1))));

    log.mono(2_000);
    let other = log.intent(8, -10, &[trial]);
    log.written(8, -10, other);
    assert_eq!(window(&log), (None, Some(Duration::from_secs(1))));

    let intent = log.intent(0, -10, &[trial]);
    assert_eq!(
        window(&log),
        (
            Some(TargetWrite::Intended(intent)),
            Some(Duration::from_secs(1))
        )
    );
    let mut unwritten = Log {
        events: log.events.clone(),
        boot: log.boot.clone(),
        mono_ms: log.mono_ms,
        wall: log.wall,
    };
    unwritten.mono(3_000).readback(0, -10, -10, intent);
    assert_eq!(
        window(&unwritten).1,
        Some(Duration::from_secs(1)),
        "a readback without the write does not start the window"
    );

    log.mono(3_000);
    let write = log.written(0, -10, intent);
    assert_eq!(
        window(&log),
        (
            Some(TargetWrite::Written(write)),
            Some(Duration::from_secs(1))
        )
    );
    log.mono(4_000).readback(0, -9, -10, write);
    assert_eq!(window(&log).1, Some(Duration::from_secs(1)));
    log.mono(5_000).readback(0, -10, -10, write);
    assert_eq!(window(&log).1, Some(Duration::from_secs(5)));
}
