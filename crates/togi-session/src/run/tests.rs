use super::*;
use crate::testing::{Bench, core};
use std::num::NonZeroU32;
use togi_journal::{Payload, ShutdownReason};
use togi_machine::{
    CancellationToken, Error, Partial, Running, Signal, Stopped, Swept, TrialSpec, Trials,
};
use togi_sim::MachineDefinition;
use togi_sim::definition::{ModelDef, Weights};

fn bench() -> Bench {
    Bench::defined(&MachineDefinition {
        cores: 2,
        model: ModelDef {
            past_limit_rate: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    })
}

fn assert_clean(bench: &Bench, ended: &Ended, stop: Stop) {
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(ended.stop, Some(stop));
    assert!(ended.emergency.is_none());
    assert!(
        bench.sim.violations().is_empty(),
        "{:?}",
        bench.sim.violations()
    );
    let facts = togi_journal::State::replay(ended.journal.events()).unwrap();
    let tuner = togi_tuner::replay(ended.journal.events()).unwrap();
    let state: crate::publication::StateDocument = ended.journal.read_state().unwrap().unwrap();
    assert_eq!(state.facts, facts);
    assert_eq!(state.tuner, tuner.project());
}

#[test]
fn a_whole_session_stops_after_the_requested_clean_cycles() {
    let bench = bench();
    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
    let ended = run(input);
    assert_clean(&bench, &ended, Stop::Cycles);
    let events = ended.journal.events();
    assert!(events.iter().any(|event| matches!(&event.payload,
        Payload::TrialIntent(intent) if intent.condition == togi_machine::Condition::Together)));
    assert!(
        matches!(&events.last().unwrap().payload, Payload::Shutdown(shutdown)
        if shutdown.reason == ShutdownReason::Cycles)
    );
}

struct BrokenLog;

impl std::io::Write for BrokenLog {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("log full"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn an_event_log_that_cannot_be_written_is_reported_and_never_stops_the_run() {
    let bench = bench();
    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
    input.log = Some(Box::new(BrokenLog));
    let ended = run(input);
    assert!(
        matches!(ended.errors.as_slice(), [SessionError::Log(_)]),
        "{:?}",
        ended.errors
    );
    assert_eq!(ended.stop, Some(Stop::Cycles));
    assert!(ended.emergency.is_none());
    let facts = togi_journal::State::replay(ended.journal.events()).unwrap();
    let state: crate::publication::StateDocument = ended.journal.read_state().unwrap().unwrap();
    assert_eq!(state.facts, facts);
}

#[test]
fn a_whole_session_stops_at_the_first_confirmed_result() {
    let bench = bench();
    let mut input = bench.input();
    input.policy.first_result = true;
    let ended = run(input);
    assert_clean(&bench, &ended, Stop::FirstResult);
    assert!(
        togi_tuner::replay(ended.journal.events())
            .unwrap()
            .first_result()
    );
}

struct CancelOnEnd {
    inner: Box<dyn Trials>,
    cancel: CancellationToken,
}

impl Trials for CancelOnEnd {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        stopped: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        self.inner.start(cancel, spec, stopped)
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        self.inner.ended(id)?;
        self.cancel.cancel();
        Ok(())
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = togi_machine::TrialConditions> + 'a> {
        self.inner.samples(id)
    }
}

#[test]
fn signal_stops_cleanly_and_drains_the_completed_trial_failure() {
    let bench = bench();
    bench
        .sim
        .script_trial(
            "0001",
            togi_sim::model::Script {
                signal: Some(Signal::ComputationError),
                at_s: 1.0,
                core: core(0),
                reset: None,
                then_crash: false,
            },
        )
        .unwrap();
    let mut input = bench.input();
    input.config = togi_config::Config::parse(r#"{"start_offsets":{"0":-10,"1":-10}}"#).unwrap();
    input.machine.trials = Box::new(CancelOnEnd {
        inner: input.machine.trials,
        cancel: input.cancel.clone(),
    });
    let ended = run(input);
    assert_clean(&bench, &ended, Stop::Signal);
    let events = ended.journal.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload, Payload::TrialIntent(_)))
            .count(),
        1
    );
    let end = events
        .iter()
        .position(|event| matches!(event.payload, Payload::TrialEnd(_)))
        .unwrap();
    let failure = events
        .iter()
        .position(|event| matches!(event.payload, Payload::Failure(_)))
        .unwrap();
    let backoff = events
        .iter()
        .position(|event| {
            matches!(&event.payload,
        Payload::TunerDecision(decision) if decision.core == core(0))
        })
        .unwrap();
    assert!(end < failure && failure < backoff);
    assert!(togi_tuner::replay(events).unwrap().drain().is_none());
}

#[test]
fn a_whole_session_dispatches_a_failure_at_zero_dead_end() {
    let bench = bench();
    let mut input = bench.input();
    input.config = togi_config::Config::parse(r#"{"start_offsets":{"0":0,"1":0}}"#).unwrap();
    bench
        .sim
        .script_trial(
            "0001",
            togi_sim::model::Script {
                signal: Some(Signal::ComputationError),
                at_s: 1.0,
                core: core(0),
                reset: None,
                then_crash: false,
            },
        )
        .unwrap();
    let ended = run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert!(
        matches!(ended.stop, Some(Stop::DeadEnd { ref dead_end, .. })
        if dead_end.condition == togi_journal::DeadEndCondition::FailureAtZero)
    );
    assert!(bench.sim.violations().is_empty());
}

#[test]
fn a_failure_at_zero_in_the_loop_restores_every_core_to_zero_and_ends_in_its_shutdown() {
    let bench = Bench::defined(&MachineDefinition {
        cores: 2,
        bios: Some(vec![-20, -20]),
        model: ModelDef {
            past_limit_rate: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    });
    let mut input = bench.input();
    input.config = togi_config::Config::parse(r#"{"start_offsets":{"0":0,"1":-10}}"#).unwrap();
    bench
        .sim
        .script_trial(
            "0001",
            togi_sim::model::Script {
                signal: Some(Signal::ComputationError),
                at_s: 1.0,
                core: core(0),
                reset: None,
                then_crash: false,
            },
        )
        .unwrap();
    let ended = run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert!(
        matches!(ended.stop, Some(Stop::DeadEnd { ref dead_end, .. })
        if dead_end.condition == togi_journal::DeadEndCondition::FailureAtZero),
        "{:?}",
        ended.stop
    );
    let events = ended.journal.events();
    let restored = events.iter().rev().find_map(|event| match &event.payload {
        Payload::ProfileRestored(restored) => Some(restored.offsets),
        _ => None,
    });
    if let Some(restored) = restored {
        assert!(
            restored.iter().all(|(_, offset)| offset.get() == 0),
            "{restored:?}"
        );
    }
    assert!(matches!(
        &events.last().unwrap().payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::DeadEnd
    ));
    let mut smu = bench.sim.seams().smu;
    assert_eq!(smu.offset(core(0)).unwrap().0, 0);
    assert_eq!(smu.offset(core(1)).unwrap().0, 0);
    assert!(bench.sim.violations().is_empty());
}

#[test]
fn together_trials_carry_the_checking_profile_the_tuner_stands_at() {
    let bench = Bench::defined(&MachineDefinition {
        cores: 2,
        model: ModelDef {
            signals: Weights::from([(Signal::ComputationError, 1.0)]),
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    });
    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
    let ended = run(input);
    assert_clean(&bench, &ended, Stop::Cycles);
    let events = ended.journal.events();
    let mut together = 0;
    for (at, event) in events.iter().enumerate() {
        let offsets = match &event.payload {
            Payload::TrialIntent(intent)
                if intent.condition == togi_machine::Condition::Together =>
            {
                &intent.profile
            }
            Payload::ProfileApplied(applied)
                if applied.condition == togi_machine::Condition::Together =>
            {
                &applied.offsets
            }
            _ => continue,
        };
        let checking = togi_tuner::replay(&events[..at])
            .unwrap()
            .profile()
            .unwrap();
        assert_eq!(*offsets, checking.offsets(), "event #{at}");
        together += 1;
    }
    assert!(together > 0);
}

#[test]
fn a_tuner_without_a_next_action_ends_the_run_with_a_typed_error() {
    let bench = bench();
    let mut session = crate::testing::swept(&bench);
    let error = session.tune().unwrap_err();
    assert!(
        matches!(
            error,
            SessionError::NotReady(togi_tuner::NotReady::NoPhase(_))
        ),
        "{error:?}"
    );
    let ended = session.close(Err(error));
    assert!(matches!(
        ended.errors.first(),
        Some(SessionError::NotReady(_))
    ));
    assert!(ended.emergency.is_none());
    assert!(bench.sim.violations().is_empty());
}

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Where a [`Rigged`] workload misbehaves; it always counts its joins.
#[derive(Clone, Copy, Default)]
struct Rigged {
    panic_in_started: bool,
    panic_in_wait: bool,
    /// What its join reports instead of stopping the workload.
    stop_error: Option<fn() -> Error>,
}

struct RiggedTrials {
    inner: Box<dyn Trials>,
    rigged: Rigged,
    joins: Arc<AtomicUsize>,
}

struct RiggedRunning {
    inner: Box<dyn Running>,
    rigged: Rigged,
    joins: Arc<AtomicUsize>,
}

impl Trials for RiggedTrials {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        stopped: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        Ok(Box::new(RiggedRunning {
            inner: self.inner.start(cancel, spec, stopped)?,
            rigged: self.rigged,
            joins: self.joins.clone(),
        }))
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        self.inner.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = togi_machine::TrialConditions> + 'a> {
        self.inner.samples(id)
    }
}

thread_local! {
    /// Set by a workload whose `started` panics, for a hook that must strike the cleanup after it.
    static STARTED_PANICKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Running for RiggedRunning {
    fn started(&self) -> togi_machine::Started {
        if self.rigged.panic_in_started {
            STARTED_PANICKED.set(true);
            panic!("a bug reading the start");
        }
        self.inner.started()
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        reporter: &mut dyn togi_machine::Reporter,
    ) -> Result<togi_machine::TrialResult, Partial<togi_machine::TrialResult>> {
        assert!(!self.rigged.panic_in_wait, "a bug while the trial runs");
        self.inner.wait(cancel, reporter)
    }

    fn stop(self: Box<Self>) -> Result<Stopped, Error> {
        self.joins.fetch_add(1, Ordering::SeqCst);
        match self.rigged.stop_error {
            Some(error) => Err(error()),
            None => self.inner.stop(),
        }
    }
}

fn two_cores_at_minus_ten() -> Bench {
    Bench::defined(&MachineDefinition {
        cores: 2,
        bios: Some(vec![-10, -10]),
        ..MachineDefinition::default()
    })
}

/// The run of `bench` with its workload rigged and `on_append` hooked in; returns the joins of the workload.
fn run_rigged(
    bench: &Bench,
    rigged: Rigged,
    on_append: Option<crate::AppendHook>,
) -> (Ended, Arc<AtomicUsize>) {
    let mut input = bench.input();
    let joins = Arc::new(AtomicUsize::new(0));
    input.machine.trials = Box::new(RiggedTrials {
        inner: bench.sim.seams().trials,
        rigged,
        joins: joins.clone(),
    });
    input.on_append = on_append;
    (run(input), joins)
}

fn restored(ended: &Ended) -> bool {
    ended
        .journal
        .events()
        .iter()
        .any(|event| event.kind() == togi_journal::EventKind::ProfileRestored)
}

fn assert_offsets(bench: &Bench, value: i64) {
    let mut smu = bench.sim.seams().smu;
    for id in 0..2 {
        assert_eq!(smu.offset(core(id)).unwrap().0, value, "core {id}");
    }
}

fn assert_handed_back(ended: &mut Ended, message: &str) {
    let panic = ended.panic.take().expect("the panic is handed back");
    assert_eq!(panic.message(), message);
    assert!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, SessionError::Panicked(seen) if seen == message)),
        "{:?}",
        ended.errors
    );
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| panic.resume())).is_err(),
        "resuming propagates the original panic"
    );
}

fn panics_at(kind: togi_journal::EventKind) -> crate::AppendHook {
    Box::new(move |event| assert!(event.kind() != kind, "a bug taking in {kind:?}"))
}

#[test]
fn a_panic_while_a_trial_runs_joins_its_teardown_and_restores_the_offsets_before_it_is_handed_back()
{
    let bench = two_cores_at_minus_ten();
    let rigged = Rigged {
        panic_in_wait: true,
        ..Rigged::default()
    };
    let (mut ended, joins) = run_rigged(&bench, rigged, None);
    assert_eq!(joins.load(Ordering::SeqCst), 1);
    assert_handed_back(&mut ended, "a bug while the trial runs");
    assert_offsets(&bench, -10);
    assert!(restored(&ended));
}

/// `Started` panics with the workload running.
#[test]
fn a_panic_after_the_workload_started_and_before_its_wait_still_joins_it_and_restores_the_offsets()
{
    let bench = two_cores_at_minus_ten();
    let rigged = Rigged {
        panic_in_started: true,
        ..Rigged::default()
    };
    let (mut ended, joins) = run_rigged(&bench, rigged, None);
    assert_eq!(
        joins.load(Ordering::SeqCst),
        1,
        "the workload was joined once"
    );
    assert_handed_back(&mut ended, "a bug reading the start");
    assert_offsets(&bench, -10);
    assert!(restored(&ended));
    assert!(
        !ended
            .journal
            .events()
            .iter()
            .any(|event| event.kind() == togi_journal::EventKind::Shutdown),
        "no clean shutdown is claimed after a panic"
    );
}

/// `Started` panics with the workload running, and cleanup panics again taking in the restoration's
/// intent. The offsets are zeroed with readback, the first panic is handed back and the second is reported.
#[test]
fn a_second_panic_taking_in_the_restoration_intent_after_a_started_panic_gets_the_emergency_and_reports_both()
 {
    let bench = two_cores_at_minus_ten();
    let rigged = Rigged {
        panic_in_started: true,
        ..Rigged::default()
    };
    let hook: crate::AppendHook = Box::new(|event| {
        assert!(
            !(STARTED_PANICKED.get() && event.kind() == togi_journal::EventKind::SmuIntent),
            "a bug taking in the restoration intent"
        );
    });
    let (mut ended, joins) = run_rigged(&bench, rigged, Some(hook));
    assert_eq!(joins.load(Ordering::SeqCst), 1);
    assert!(
        ended.errors.iter().any(|error| matches!(
            error,
            SessionError::Panicked(seen) if seen == "a bug taking in the restoration intent"
        )),
        "{:?}",
        ended.errors
    );
    assert_handed_back(&mut ended, "a bug reading the start");
    assert_emergency_zeroed(&bench, &ended);
}

#[test]
fn a_panic_taking_in_the_trial_start_joins_the_workload_and_answers_with_the_emergency() {
    let bench = two_cores_at_minus_ten();
    let hook = panics_at(togi_journal::EventKind::TrialStart);
    let (mut ended, joins) = run_rigged(&bench, Rigged::default(), Some(hook));
    assert_eq!(
        joins.load(Ordering::SeqCst),
        1,
        "the workload was joined once"
    );
    assert_handed_back(&mut ended, "a bug taking in TrialStart");
    assert_emergency_zeroed(&bench, &ended);
}

/// What a latched appender leaves: the emergency zeroed the machine, and nothing was restored or published from the
/// folds that missed a durable event.
fn assert_emergency_zeroed(bench: &Bench, ended: &Ended) {
    assert!(
        matches!(
            ended.emergency.as_ref().map(|outcome| &outcome.response),
            Some(crate::Response::Zeroed(zeroed)) if zeroed.is_clean()
        ),
        "{:?}",
        ended.emergency
    );
    assert_offsets(bench, 0);
    assert!(!restored(ended));
    assert!(
        ended
            .journal
            .read_state::<crate::publication::StateDocument>()
            .unwrap()
            .is_none(),
        "no state is published from stale folds"
    );
    assert!(ended.capsule.is_none());
}

#[test]
fn a_panic_taking_in_an_event_after_it_is_durable_latches_the_appender_instead_of_restoring_from_stale_folds()
 {
    let bench = two_cores_at_minus_ten();
    let hook = panics_at(togi_journal::EventKind::TrialProgress);
    let (mut ended, joins) = run_rigged(&bench, Rigged::default(), Some(hook));
    assert!(joins.load(Ordering::SeqCst) >= 1, "the workload was joined");
    assert_handed_back(&mut ended, "a bug taking in TrialProgress");
    assert_emergency_zeroed(&bench, &ended);
}

#[test]
fn a_panic_whose_teardown_cannot_be_confirmed_reports_both_and_restores_nothing() {
    for (stop_error, crashed) in [
        (
            (|| Error::Containment("workload still running".into())) as fn() -> Error,
            false,
        ),
        (|| Error::Crashed, true),
    ] {
        let bench = two_cores_at_minus_ten();
        let rigged = Rigged {
            panic_in_wait: true,
            stop_error: Some(stop_error),
            ..Rigged::default()
        };
        let (mut ended, joins) = run_rigged(&bench, rigged, None);
        assert_eq!(joins.load(Ordering::SeqCst), 1);
        assert!(
            matches!(
                ended.errors.as_slice(),
                [SessionError::Panicked(_), SessionError::Machine(error)]
                    if matches!(error, Error::Crashed) == crashed
            ),
            "{:?}",
            ended.errors
        );
        assert_eq!(ended.crashed(), crashed);
        assert!(ended.panic.take().is_some());
        assert!(
            !restored(&ended),
            "an unconfirmed teardown restores nothing"
        );
    }
}

struct PanicsWhileSweeping;

impl Trials for PanicsWhileSweeping {
    fn sweep(&mut self, _: &CancellationToken) -> Result<Swept, Error> {
        panic!("a bug during startup");
    }

    fn start(
        &mut self,
        _: &CancellationToken,
        _: TrialSpec,
        _: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        unreachable!("the run panicked before any trial");
    }

    fn ended(&mut self, _: &str) -> Result<(), Error> {
        Ok(())
    }

    fn samples<'a>(
        &'a self,
        _: &'a str,
    ) -> Box<dyn Iterator<Item = togi_machine::TrialConditions> + 'a> {
        Box::new(std::iter::empty())
    }
}

#[test]
fn a_panic_during_startup_is_handed_back_with_the_journal_closed_out_and_no_offset_written() {
    let bench = Bench::defined(&MachineDefinition {
        cores: 2,
        bios: Some(vec![-10, -10]),
        ..MachineDefinition::default()
    });
    let mut input = bench.input();
    input.machine.trials = Box::new(PanicsWhileSweeping);
    let mut ended = run(input);
    let panic = ended.panic.take().expect("the panic is handed back");
    assert_eq!(panic.message(), "a bug during startup");
    assert!(matches!(
        ended.errors.as_slice(),
        [SessionError::Panicked(message)] if message == "a bug during startup"
    ));
    assert!(
        !ended
            .journal
            .events()
            .iter()
            .any(|event| event.kind() == togi_journal::EventKind::Shutdown),
        "no clean shutdown is claimed for a run that never proved the workload stopped"
    );
    assert!(bench.sim.violations().is_empty());
    let mut smu = bench.sim.seams().smu;
    for id in 0..2 {
        assert_eq!(smu.offset(core(id)).unwrap().0, -10, "core {id} untouched");
    }
}
