use std::num::NonZeroU32;

use togi_journal::{
    CheckingCycle, CycleEvent, DeadEnd, DeadEndCondition, DeepeningRound, Payload, RoundEvent,
    ShutdownReason, TunerWarning, WarningKind,
};
use togi_machine::{
    BiosContext, BootId, Check, CoreInfo, CoreRank, Error, Host, Partial, SmuValidated,
};
use togi_tuner::{Action, DecisionAction, DecisionPayload};

use super::*;
use crate::session::{Control, Stop, StopPolicy};
use crate::testing::{Bench, begun, core, run_action};

fn count(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

fn cycle(event: CycleEvent) -> DecisionPayload {
    DecisionPayload::CheckingCycle(CheckingCycle {
        cycle: count(1),
        event,
        passed: false,
        full: false,
        missing: Vec::new(),
        steps: Vec::new(),
        reason: None,
    })
}

fn round(event: RoundEvent) -> DecisionPayload {
    DecisionPayload::DeepeningRound(DeepeningRound {
        round: count(1),
        event,
        base: PerCore::default(),
        base_seq: None,
        target: PerCore::default(),
        profile: PerCore::default(),
        cores: togi_machine::CoreSet::default(),
        ranking: Vec::new(),
        trials: None,
        trial_s: None,
        passed: false,
        reason: None,
    })
}

fn warning() -> DecisionPayload {
    DecisionPayload::TunerWarning(TunerWarning {
        warning: WarningKind::Monotonicity,
        trial: None,
        passes: Vec::new(),
        detail: "test".to_owned(),
    })
}

#[test]
fn a_stop_policy_is_reached_only_where_a_cycle_or_round_starts() {
    let cycles = StopPolicy {
        cycles: Some(count(2)),
        first_result: false,
    };
    assert_eq!(
        cycles.reached(&cycle(CycleEvent::Start), 2, false),
        Some(Stop::Cycles)
    );
    assert_eq!(
        cycles.reached(&cycle(CycleEvent::Start), 3, false),
        Some(Stop::Cycles)
    );
    assert_eq!(cycles.reached(&cycle(CycleEvent::Start), 1, false), None);
    assert_eq!(cycles.reached(&cycle(CycleEvent::End), 2, false), None);
    assert_eq!(cycles.reached(&round(RoundEvent::Start), 2, false), None);
    assert_eq!(cycles.reached(&warning(), 2, false), None);

    let first = StopPolicy {
        cycles: None,
        first_result: true,
    };
    assert_eq!(
        first.reached(&cycle(CycleEvent::Start), 0, true),
        Some(Stop::FirstResult)
    );
    assert_eq!(
        first.reached(&round(RoundEvent::Start), 0, true),
        Some(Stop::FirstResult)
    );
    assert_eq!(first.reached(&round(RoundEvent::End), 0, true), None);
    assert_eq!(first.reached(&cycle(CycleEvent::End), 0, true), None);
    assert_eq!(first.reached(&cycle(CycleEvent::Start), 0, false), None);
    assert_eq!(
        StopPolicy::default().reached(&cycle(CycleEvent::Start), 9, true),
        None
    );

    let both = StopPolicy {
        cycles: Some(count(1)),
        first_result: true,
    };
    assert_eq!(
        both.reached(&cycle(CycleEvent::Start), 1, true),
        Some(Stop::Cycles)
    );
}

fn tuner_values_stop(
    policy: StopPolicy,
    clean_cycles: u32,
    payload: DecisionPayload,
) -> Option<Stop> {
    let bench = Bench::new(2);
    let mut session = begun(&bench);
    session.policy = policy;
    let before = session.appender().journal().events().len();
    let profile = crate::testing::tuned_values(&session);
    let outcome = session
        .execute(
            Action::Decide(DecisionAction::new(payload, Vec::new())),
            &TunerValues {
                profile: Some(&profile),
                profile_seq: None,
                clean_cycles,
                first_result: false,
            },
        )
        .unwrap();
    assert_eq!(session.appender().journal().events().len(), before);
    match outcome {
        Control::Stop(stop) => Some(stop),
        Control::Continue => None,
    }
}

#[test]
fn a_cycle_start_the_policy_reaches_stops_the_run_without_recording_it() {
    let policy = StopPolicy {
        cycles: Some(count(2)),
        first_result: false,
    };
    assert_eq!(
        tuner_values_stop(policy, 2, cycle(CycleEvent::Start)),
        Some(Stop::Cycles)
    );
}

#[test]
fn a_decision_is_recorded_with_the_events_it_cites() {
    let bench = Bench::new(2);
    let mut session = begun(&bench);
    let cited = session.appender().journal().events().last().unwrap().seq;
    let outcome = run_action(
        &mut session,
        Action::Decide(DecisionAction::new(warning(), vec![cited])),
    )
    .unwrap();
    assert_eq!(outcome, Control::Continue);
    let last = session.appender().journal().events().last().unwrap();
    assert!(matches!(last.payload, Payload::TunerWarning(_)));
    assert_eq!(last.cause, [cited]);
}

#[test]
fn a_cancelled_run_records_no_decision_and_stops_for_the_signal() {
    let bench = Bench::new(2);
    let mut session = begun(&bench);
    session.cancel.cancel();
    let before = session.appender().journal().events().len();
    let outcome = run_action(
        &mut session,
        Action::Decide(DecisionAction::new(warning(), Vec::new())),
    )
    .unwrap();
    assert_eq!(outcome, Control::Stop(Stop::Signal));
    assert_eq!(session.appender().journal().events().len(), before);
    assert_eq!(
        session.shutdown.as_ref().map(|shutdown| shutdown.reason),
        Some(ShutdownReason::Signal)
    );
}

#[test]
fn a_dead_end_decision_ends_the_run_with_the_dead_end_citing_its_evidence() {
    let bench = Bench::new(2);
    let mut session = begun(&bench);
    let cited = session.appender().journal().events().last().unwrap().seq;
    let dead_end = DeadEnd {
        condition: DeadEndCondition::FailureAtZero,
        core: Some(core(0)),
        detail: "core 0 fails at zero".to_owned(),
        action: None,
    };
    let outcome = run_action(
        &mut session,
        Action::Decide(DecisionAction::new(dead_end.clone(), vec![cited])),
    )
    .unwrap();
    match outcome {
        Control::Stop(Stop::DeadEnd {
            dead_end: stopped,
            causes,
            ..
        }) => {
            assert_eq!(
                (stopped.condition, stopped.core),
                (dead_end.condition, dead_end.core)
            );
            assert!(stopped.action.is_some());
            assert_eq!(causes, [cited]);
        }
        other => panic!("{other:?}"),
    }
    let last = session.appender().journal().events().last().unwrap();
    assert!(matches!(last.payload, Payload::DeadEnd(_)));
    assert_eq!(last.cause, [cited]);
}

type Ranks = Result<Vec<CoreRank>, (Vec<CoreRank>, fn() -> Error)>;

struct Ranked {
    inner: Box<dyn Host>,
    ranks: Ranks,
}

impl Host for Ranked {
    fn boot_id(&mut self) -> Result<BootId, Error> {
        self.inner.boot_id()
    }

    fn topology(&mut self) -> Result<Vec<CoreInfo>, Error> {
        self.inner.topology()
    }

    fn validate_smu(&mut self) -> Result<SmuValidated, Error> {
        self.inner.validate_smu()
    }

    fn bios_context(&mut self) -> Result<BiosContext, Error> {
        self.inner.bios_context()
    }

    fn ranking(&mut self) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> {
        match &self.ranks {
            Ok(ranks) => Ok(ranks.clone()),
            Err((value, error)) => Err(Partial {
                value: value.clone(),
                error: error(),
            }),
        }
    }

    fn preflight(&mut self) -> Vec<Check> {
        self.inner.preflight()
    }

    fn watchdog(&mut self) -> Check {
        self.inner.watchdog()
    }
}

fn ranks(values: &[i64]) -> Vec<CoreRank> {
    (0..)
        .zip(values)
        .map(|(n, &value)| CoreRank {
            core: core(n),
            value,
        })
        .collect()
}

fn recorded_ranking(cores: i64, ranks: Ranks) -> togi_journal::HostRanking {
    let bench = Bench::new(cores);
    let mut session = begun(&bench);
    let inner = std::mem::replace(&mut session.host, bench.sim.seams().host);
    session.host = Box::new(Ranked { inner, ranks });
    assert_eq!(
        run_action(&mut session, Action::ReadRanking).unwrap(),
        Control::Continue
    );
    match &session
        .appender()
        .journal()
        .events()
        .last()
        .unwrap()
        .payload
    {
        Payload::HostRanking(ranking) => ranking.clone(),
        other => panic!("{other:?}"),
    }
}

fn order(ranking: &togi_journal::HostRanking) -> Vec<u8> {
    ranking.ranking.iter().map(|core| core.get()).collect()
}

#[test]
fn a_ranking_the_host_gives_orders_cores_best_first_and_ties_by_core_id() {
    let ranking = recorded_ranking(4, Ok(ranks(&[2, 5, 5, 1])));
    assert_eq!(order(&ranking), [1, 2, 0, 3]);
    assert_eq!(ranking.detail, None);
    assert_eq!(ranking.values.get(core(1)), Some(5));
}

#[test]
fn a_ranking_without_a_value_for_every_core_falls_back_to_core_ids() {
    let ranking = recorded_ranking(4, Ok(ranks(&[4])));
    assert_eq!(order(&ranking), [0, 1, 2, 3]);
    assert_eq!(
        ranking.detail.as_deref(),
        Some("ranking has 1 values for 4 cores")
    );
}

#[test]
fn a_ranking_every_core_shares_falls_back_to_core_ids() {
    let ranking = recorded_ranking(4, Ok(ranks(&[7, 7, 7, 7])));
    assert_eq!(order(&ranking), [0, 1, 2, 3]);
    assert_eq!(ranking.detail.as_deref(), Some("every core ranks 7"));
}

#[test]
fn an_unreadable_ranking_falls_back_to_core_ids_and_says_why() {
    let ranking = recorded_ranking(
        4,
        Err((ranks(&[9, 1]), || Error::Other("no preferred cores".into()))),
    );
    assert_eq!(order(&ranking), [0, 1, 2, 3]);
    assert_eq!(ranking.detail.as_deref(), Some("no preferred cores"));
}

#[test]
fn a_cancelled_run_reads_no_ranking() {
    let bench = Bench::new(2);
    let mut session = begun(&bench);
    session.cancel.cancel();
    let before = session.appender().journal().events().len();
    assert_eq!(
        run_action(&mut session, Action::ReadRanking).unwrap(),
        Control::Stop(Stop::Signal)
    );
    assert_eq!(session.appender().journal().events().len(), before);
}
