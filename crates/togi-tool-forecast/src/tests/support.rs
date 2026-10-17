use std::num::NonZeroU32;

use jiff::{SignedDuration, Timestamp};
use togi_config::Config;
use togi_journal::{
    BootLeaveReason, Build, CommandReset, ConfigLoaded, CrashDetected, DeadEnd, DeadEndCondition,
    Event, JournalTorn, Mce, Payload, ResetTarget, SCHEMA, Seq, SessionBaseline, SessionId,
    SessionStart, Shutdown, ShutdownReason, Stamp, StateRebuilt, TrialId, TrialIntent,
};
use togi_machine::{
    BankType, BootId, CcdId, Condition, CoreId, CoreInfo, CoreSet, CpuId, ObservedOffset, PerCore,
    Regime, TargetOffset,
};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use crate::outcome::Replay;
use crate::{Outcome, Run, Session, Split, Status};

pub(super) const A: &str = "20261007T000000Z";
pub(super) const B: &str = "20261007T010000Z";

const EPOCH: Timestamp = Timestamp::constant(1_791_331_200, 0);

pub(super) fn id(text: &str) -> SessionId {
    text.parse().unwrap()
}

pub(super) fn core(n: i64) -> CoreId {
    CoreId::new(n).unwrap()
}

pub(super) fn build(rev: &str) -> Build {
    Build {
        version: "1.0.0".into(),
        rev: Some(rev.to_owned()),
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: 0,
        evidence: EVIDENCE_EPOCH,
    }
}

pub(super) fn offsets<T: Copy + Default>(values: &[(i64, T)]) -> PerCore<T> {
    values.iter().map(|&(n, value)| (core(n), value)).collect()
}

pub(super) fn observed(values: &[i64]) -> PerCore<ObservedOffset> {
    values
        .iter()
        .enumerate()
        .map(|(n, &value)| (core(i64::try_from(n).unwrap()), ObservedOffset(value)))
        .collect()
}

pub(super) fn start_with(session: &str, build: Build) -> Payload {
    Payload::SessionStart(SessionStart {
        build,
        session: id(session),
        cores: vec![
            CoreInfo {
                core: core(0),
                ccd: CcdId::new(0).unwrap(),
                cpus: Vec::new(),
            },
            CoreInfo {
                core: core(1),
                ccd: CcdId::new(1).unwrap(),
                cpus: Vec::new(),
            },
        ],
    })
}

pub(super) fn baseline(values: [i64; 2]) -> Payload {
    Payload::SessionBaseline(SessionBaseline {
        offsets: observed(&values),
    })
}

/// A session's opening: `session.start` by the build at `rev`, then a baseline of -10 and -20.
pub(super) fn start(session: &str, rev: &str) -> Vec<Payload> {
    vec![start_with(session, build(rev)), baseline([-10, -20])]
}

pub(super) fn crash() -> Payload {
    Payload::CrashDetected(CrashDetected {
        previous_boot: BootId::from("boot".to_owned()),
        in_flight: None,
        stray: false,
        condition: None,
        reset_reason: None,
        reset_reason_raw: Vec::new(),
        inconclusive: false,
        pstore: None,
    })
}

pub(super) fn shutdown_cycles() -> Payload {
    Payload::Shutdown(Shutdown {
        reason: ShutdownReason::Cycles,
        kernel_cursor: None,
        kernel_error: None,
        cycles: NonZeroU32::new(1),
    })
}

pub(super) fn shutdown_signal() -> Payload {
    Payload::Shutdown(Shutdown {
        reason: ShutdownReason::Signal,
        kernel_cursor: None,
        kernel_error: None,
        cycles: None,
    })
}

pub(super) fn leave_reason() -> Payload {
    Payload::BootLeaveReason(BootLeaveReason {
        reason_id: "id".to_owned(),
        restart_limit_count: 1,
        reason: "watchdog".to_owned(),
    })
}

pub(super) fn mce() -> Payload {
    Payload::Mce(Mce {
        cpu: CpuId::from(0),
        core: Some(core(0)),
        bank: 0,
        bank_type: BankType::LoadStore,
        corrected: true,
        monotonic_ns: None,
        from_boot: None,
        trial: None,
        between_trials: true,
        lines: Vec::new(),
    })
}

pub(super) fn state_rebuilt() -> Payload {
    Payload::StateRebuilt(StateRebuilt {
        fields: vec!["last_seq".to_owned()],
    })
}

pub(super) fn journal_torn() -> Payload {
    Payload::JournalTorn(JournalTorn {
        offset: 1024,
        bytes_hex: "7b22".to_owned(),
    })
}

pub(super) fn command_reset() -> Payload {
    Payload::CommandReset(CommandReset {
        target: ResetTarget::Core(core(0)),
    })
}

pub(super) fn dead_end() -> Payload {
    Payload::DeadEnd(DeadEnd {
        condition: DeadEndCondition::FailureAtZero,
        core: None,
        detail: "test".to_owned(),
        action: None,
    })
}

pub(super) fn config_loaded(rev: &str) -> Payload {
    config_loaded_by(build(rev))
}

pub(super) fn config_loaded_by(build: Build) -> Payload {
    Payload::ConfigLoaded(ConfigLoaded {
        build,
        config: Config::default().snapshot(),
        path: "/etc/togi/config.json".to_owned(),
        file: false,
        kernel_cursor: None,
        kernel_error: None,
    })
}

/// A `trial.intent` of the first R1 workload on core 0; `hunt` makes it a hunt's.
pub(super) fn intent(trial: &str, hunt: Option<u32>) -> Payload {
    Payload::TrialIntent(TrialIntent {
        trial: TrialId::from(trial.to_owned()),
        kernel_cursor: None,
        kernel_error: None,
        profile: offsets(&[(0, TargetOffset::ZERO)]),
        core: Some(core(0)),
        cores: CoreSet::default(),
        offset: None,
        regime: Regime::R1,
        workload: togi_machine::workloads(Regime::R1)[0].id(),
        duration_s: 60,
        condition: Condition::Alone,
        activity: None,
        retry: false,
        cycle: None,
        step: None,
        hunt: hunt.and_then(NonZeroU32::new),
        group: None,
        round: None,
        rerun: false,
    })
}

/// Events an hour apart, numbered from `seq`.
pub(super) fn events(seq: u64, payloads: impl IntoIterator<Item = Payload>) -> Vec<Event> {
    payloads
        .into_iter()
        .zip(seq..)
        .map(|(payload, seq)| {
            let time = EPOCH
                .checked_add(SignedDuration::from_hours(i64::try_from(seq).unwrap()))
                .unwrap();
            Event::new(
                Seq::new(seq).unwrap(),
                Stamp { time, mono: None },
                BootId::from("boot".to_owned()),
                Vec::new(),
                payload,
            )
        })
        .collect()
}

/// Events numbered from `seq`, each at its hours after the epoch in its boot.
pub(super) fn timed<'a>(
    seq: u64,
    events: impl IntoIterator<Item = (f64, &'a str, Payload)>,
) -> Vec<Event> {
    events
        .into_iter()
        .zip(seq..)
        .map(|((hours, boot, payload), seq)| {
            Event::new(
                Seq::new(seq).unwrap(),
                Stamp {
                    time: EPOCH
                        .checked_add(SignedDuration::from_secs_f64(hours * 3600.0))
                        .unwrap(),
                    mono: None,
                },
                BootId::from(boot.to_owned()),
                Vec::new(),
                payload,
            )
        })
        .collect()
}

/// A session of `payloads` numbered from `seq`.
pub(super) fn session(seq: u64, payloads: impl IntoIterator<Item = Payload>) -> Session {
    Session::from_events(events(seq, payloads)).unwrap()
}

/// `base` followed by `payloads` numbered from `seq`.
pub(super) fn extend(
    base: &[Event],
    seq: u64,
    payloads: impl IntoIterator<Item = Payload>,
) -> Session {
    let mut all = base.to_vec();
    all.extend(events(seq, payloads));
    Session::from_events(all).unwrap()
}

pub(super) fn run(status: Status, hours: f64, crashes: u32, hunts: u32, profile: &[i64]) -> Run {
    Run {
        machine: "m.json".to_owned(),
        seed: 0,
        split: Split::Dev,
        outcome: outcome(status, hours, crashes, hunts, profile),
    }
}

pub(super) fn outcome(
    status: Status,
    hours: f64,
    crashes: u32,
    hunts: u32,
    profile: &[i64],
) -> Outcome {
    Outcome {
        status,
        hours,
        crashes,
        hunts,
        profile: observed(profile),
        depth: profile.iter().sum(),
    }
}

/// A tuner that counts the passed `checking.cycle` ends it has folded as clean cycles, and has no checking
/// profile.
#[derive(Default)]
pub(super) struct Counting {
    clean: u32,
}

impl Replay for Counting {
    fn fold(&mut self, event: &Event) -> Result<(), togi_tuner::FoldError> {
        if let Payload::CheckingCycle(cycle) = &event.payload {
            if cycle.event == togi_journal::CycleEvent::End && cycle.passed {
                self.clean += 1;
            }
        }
        Ok(())
    }

    fn clean_cycles(&self) -> u32 {
        self.clean
    }

    fn profile(&self) -> Option<PerCore<ObservedOffset>> {
        None
    }
}

pub(super) fn hunt_start(hunt: u32) -> Payload {
    Payload::HuntStart(togi_journal::HuntStart {
        hunt: NonZeroU32::new(hunt).unwrap(),
        failure: Seq::FIRST,
        trial: None,
        regime: Regime::R1,
        workload: togi_machine::workloads(Regime::R1)[0].id(),
        cores: CoreSet::default(),
        duration_s: 60,
        failing: PerCore::default(),
        parked: PerCore::default(),
        parked_seq: None,
        candidates: CoreSet::default(),
        trials: 0,
        trial_s: 0,
        miss: 0.0,
        rate: 0.0,
        ranking: Vec::new(),
        reason: None,
    })
}

pub(super) fn hunt_end(hunt: u32) -> Payload {
    Payload::HuntEnd(togi_journal::HuntEnd {
        hunt: NonZeroU32::new(hunt).unwrap(),
        result: togi_journal::HuntOutcome::Direct,
        cores: CoreSet::default(),
        members: Vec::new(),
        groups: 0,
        reason: "carried".to_owned(),
    })
}

pub(super) fn cycle(cycle: u32, event: togi_journal::CycleEvent, passed: bool) -> Payload {
    Payload::CheckingCycle(togi_journal::CheckingCycle {
        cycle: NonZeroU32::new(cycle).unwrap(),
        event,
        passed,
        full: passed,
        missing: Vec::new(),
        steps: Vec::new(),
        reason: None,
    })
}
