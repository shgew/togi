use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;
use std::time::Duration;

use togi_config::Config;
use togi_journal::fs::{Fault, Op};
use togi_journal::{
    Attribution, CrashDetected, DeadEnd, DeadEndAction, DeadEndCondition, Event, EventKind,
    HuntOutcome, Payload,
};
use togi_machine::{
    BankType, BootId, CancellationToken, Condition, CpuId, Error, Kernel, KernelCursor, KernelRead,
    Partial, PstoreRecord, Regime, ResetReason, Running, Signal, Stopped, Swept, TrialSpec, Trials,
};
use togi_sim::definition::{CoreDef, JointDef, ModelDef};
use togi_sim::faults::{HookPhase, SmuOp, SmuOperation};
use togi_sim::{MachineDefinition, Simulator};
use togi_tuner::DecisionPayload;
use togi_tuningboot::{Bootloader, Environment, SavedEntry};

use crate::session::{Control, Session};
use crate::testing::{Bench, MemBoot, begun, core, put, run_plan, search_plan};
use crate::{Ended, SessionError, Stop, run};

const BIOS: [i64; 4] = [-40, -40, -20, -5];
const HELD: [i64; 4] = [-30, -29, -19, -5];
const LEFT: [i64; 4] = [-45, -45, -25, -12];
const BIOS_OF_HUNT: [i64; 4] = [-40, -40, 0, 0];
const PENDING_DETAIL: &str = "recorded backend failure";

fn quiet(cores: i64, bios: Option<Vec<i64>>) -> Bench {
    Bench::defined(&MachineDefinition {
        cores,
        bios,
        model: ModelDef {
            past_limit_rate: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    })
}

/// A bootloader that counts how often its saved entry is cleared.
struct TuningBoot {
    env: MemBoot,
    clears: Rc<Cell<u32>>,
}

impl Environment for TuningBoot {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        self.env.get(name)
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        self.env.set(values)
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        self.env.unset(names)
    }
}

impl Bootloader for TuningBoot {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        self.clears.set(self.clears.get() + 1);
        self.env.clear_saved_entry()
    }
}

/// A boot whose earlier run was killed after leaving offsets deeper than its cores' search positions.
struct SameBoot {
    bench: Bench,
    /// Set when the runs start from the tuning boot.
    clears: Option<Rc<Cell<u32>>>,
    /// The events the killed run left.
    left: usize,
}

impl SameBoot {
    fn new(pending: bool) -> Self {
        Self::killed(pending, |_, _| {})
    }

    /// The killed run also appended what `more` appends after the offsets it left.
    fn killed(pending: bool, more: impl FnOnce(&Bench, &mut Session)) -> Self {
        Self::killed_from(
            r#"{"start_offsets":{"0":-30,"1":-29,"2":-19,"3":-5}}"#,
            pending,
            more,
        )
    }

    fn killed_from(config: &str, pending: bool, more: impl FnOnce(&Bench, &mut Session)) -> Self {
        let mut bench = quiet(4, Some(BIOS.to_vec()));
        bench.config = Config::parse(config).unwrap();
        let mut session = begun(&bench);
        put(&mut session, &LEFT);
        more(&bench, &mut session);
        if pending {
            let dead_end = DeadEnd {
                condition: DeadEndCondition::NoEvidence,
                core: None,
                detail: PENDING_DETAIL.into(),
                action: Some(DeadEndAction::ClearSavedEntry),
            };
            session
                .appender
                .append(Payload::DeadEnd(dead_end), &[])
                .unwrap();
        }
        let left = session.appender().journal().events().len();
        drop(session);
        Self {
            bench,
            clears: pending.then(Rc::default),
            left,
        }
    }

    fn with_tuning_boot(mut self) -> Self {
        self.clears.get_or_insert_with(Rc::default);
        self
    }

    /// A restart of the killed process, asked to stop as soon as it may.
    fn resume(&self) -> Ended {
        let mut input = self.bench.input();
        input.cancel.cancel();
        input.bootloader = self.clears.as_ref().map(|clears| {
            Box::new(TuningBoot {
                env: MemBoot::default(),
                clears: Rc::clone(clears),
            }) as Box<dyn Bootloader>
        });
        run(input)
    }

    fn clears(&self) -> u32 {
        self.clears.as_ref().map_or(0, |clears| clears.get())
    }

    fn offsets(&self) -> Vec<i64> {
        self.bench.sim.interrupt_smu(|_: &SmuOperation| Ok(()));
        let mut smu = self.bench.sim.seams().smu;
        (0..4).map(|n| smu.offset(core(n)).unwrap().0).collect()
    }

    fn boot(&self) -> BootId {
        self.bench.sim.seams().host.boot_id().unwrap()
    }

    fn since<'a>(&self, ended: &'a Ended) -> &'a [Event] {
        &ended.journal.events()[self.left..]
    }
}

fn position(events: &[Event], kind: EventKind) -> Option<usize> {
    events.iter().position(|event| event.kind() == kind)
}

fn last_position(events: &[Event], kind: EventKind) -> Option<usize> {
    events.iter().rposition(|event| event.kind() == kind)
}

fn pending_dead_end(stop: Option<&Stop>) -> Option<&DeadEnd> {
    match stop {
        Some(Stop::DeadEnd { dead_end, .. }) => Some(dead_end),
        _ => None,
    }
}

#[test]
fn a_same_boot_resume_restores_the_safe_target_before_it_finishes_the_pending_dead_end() {
    let fixture = SameBoot::new(true);
    let ended = fixture.resume();
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(
        pending_dead_end(ended.stop.as_ref()).map(|dead_end| dead_end.detail.as_str()),
        Some(PENDING_DETAIL)
    );
    assert_eq!(fixture.offsets(), HELD);
    let events = fixture.since(&ended);
    assert_eq!(position(events, EventKind::TrialIntent), None);
    let preflight = last_position(events, EventKind::PreflightCheck).unwrap();
    let intent = position(events, EventKind::SmuIntent).unwrap();
    let restored = position(events, EventKind::ProfileRestored).unwrap();
    let entry = position(events, EventKind::BootSavedEntry).unwrap();
    let shutdown = position(events, EventKind::Shutdown).unwrap();
    assert!(preflight < intent, "{preflight} {intent}");
    assert!(
        intent < restored && restored < entry && entry < shutdown,
        "{intent} {restored} {entry} {shutdown}"
    );
    assert_eq!(fixture.clears(), 1);
}

/// Makes the `at`th SMU access from now fail as a killed process, which leaves the machine up in the same boot.
fn kill_at(fixture: &SameBoot, at: usize, expected: Option<SmuOperation>) -> Rc<Cell<bool>> {
    let fired = Rc::new(Cell::new(false));
    let seen = Rc::clone(&fired);
    let mut count = 0;
    fixture
        .bench
        .sim
        .interrupt_smu(move |operation: &SmuOperation| {
            count += 1;
            if count != at || seen.get() {
                return Ok(());
            }
            if let Some(expected) = expected {
                assert_eq!(*operation, expected, "SMU access #{at}");
            }
            seen.set(true);
            Err(Error::Crashed)
        });
    fired
}

fn recorded_accesses(fixture: &SameBoot) -> (Ended, Vec<SmuOperation>) {
    let accesses = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&accesses);
    fixture
        .bench
        .sim
        .interrupt_smu(move |operation: &SmuOperation| {
            log.borrow_mut().push(*operation);
            Ok(())
        });
    let ended = fixture.resume();
    let accesses = accesses.take();
    (ended, accesses)
}

#[test]
fn every_smu_access_window_of_the_same_boot_restoration_converges() {
    for pending in [false, true] {
        let reference = SameBoot::new(pending);
        let (ended, accesses) = recorded_accesses(&reference);
        assert!(ended.errors.is_empty(), "{:?}", ended.errors);
        let want = reference.offsets();
        assert_eq!(want, HELD);
        let reads = accesses
            .iter()
            .filter(|access| access.op == SmuOp::Read && access.phase == HookPhase::After)
            .count();
        assert!(reads >= 4 && accesses.len() > 2 * reads, "{accesses:?}");
        for (index, &access) in accesses.iter().enumerate() {
            let fixture = SameBoot::new(pending);
            let boot = fixture.boot();
            let fired = kill_at(&fixture, index + 1, Some(access));
            let killed = fixture.resume();
            assert!(fired.get() && killed.crashed(), "{:?}", killed.errors);
            drop(killed);
            for _ in 0..2 {
                let fired = kill_at(&fixture, 8, None);
                let killed = fixture.resume();
                assert!(fired.get() && killed.crashed(), "{:?}", killed.errors);
                assert_eq!(fixture.boot(), boot);
            }
            fixture.bench.sim.interrupt_smu(|_: &SmuOperation| Ok(()));
            let resumed = fixture.resume();
            assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
            assert_eq!(resumed.stop, ended.stop);
            assert_eq!(
                fixture.offsets(),
                want,
                "pending {pending}, access #{} {access:?}",
                index + 1
            );
            assert!(fixture.bench.sim.violations().is_empty());
            assert!(fixture.clears() <= 1);
        }
    }
}

#[test]
fn a_different_boot_never_reconciles_what_the_same_boot_left() {
    for pending in [false, true] {
        let fixture = SameBoot::new(pending);
        fixture.bench.sim.reboot();
        let ended = fixture.resume();
        assert!(ended.errors.is_empty(), "{:?}", ended.errors);
        assert_eq!(fixture.offsets(), BIOS);
        for event in fixture.since(&ended) {
            assert!(
                !matches!(
                    event.kind(),
                    EventKind::SmuReadback | EventKind::SmuIntent | EventKind::ProfileRestored
                ),
                "pending {pending}: {}",
                event.kind().as_str()
            );
        }
    }
}

/// A killed run that left a confirmed failure at CO 0 on core 2: drained but not recorded, or recorded as the
/// dead end the next boot has pending.
fn zero_failure(record: bool) -> SameBoot {
    SameBoot::killed_from(
        r#"{"start_offsets":{"0":-30,"1":-29,"2":0,"3":-5}}"#,
        false,
        |bench, session| {
            session.validated = Some(session.host.validate_smu().unwrap());
            bench
                .sim
                .script_trial(
                    "0001",
                    togi_sim::model::Script {
                        signal: Some(Signal::ComputationError),
                        at_s: 1.0,
                        core: core(2),
                        reset: None,
                        then_crash: false,
                    },
                )
                .unwrap();
            let plan = search_plan(session, core(2), Regime::R1);
            run_plan(session, plan).unwrap();
            if record {
                for _ in 0..16 {
                    let action = session
                        .appender()
                        .tuner()
                        .drain()
                        .expect("the failure at CO 0 owes a dead end");
                    let dead_end = matches!(action.payload, DecisionPayload::DeadEnd(_));
                    session
                        .appender
                        .append(action.payload.into(), &action.cause)
                        .unwrap();
                    if dead_end {
                        return;
                    }
                }
                panic!("no dead end among the first 16 owed decisions");
            }
        },
    )
}

fn assert_zero_dead_end_finished(fixture: &SameBoot, ended: &Ended) {
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let Some(Stop::DeadEnd { dead_end, .. }) = ended.stop.as_ref() else {
        panic!("{:?}", ended.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::FailureAtZero);
    assert_eq!(dead_end.core, Some(core(2)));
    let events = fixture.since(ended);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind() == EventKind::DeadEnd)
            .count(),
        usize::from(
            position(&ended.journal.events()[..fixture.left], EventKind::DeadEnd).is_none()
        )
    );
    assert_eq!(
        ended.journal.events().last().unwrap().kind(),
        EventKind::Shutdown
    );
}

#[test]
fn a_same_boot_resume_finishes_a_dead_end_it_drains_without_deepening_any_core() {
    let fixture = zero_failure(false);
    let ended = fixture.resume();
    assert_zero_dead_end_finished(&fixture, &ended);
    let events = fixture.since(&ended);
    let recorded = position(events, EventKind::DeadEnd).unwrap();
    let shutdown = position(events, EventKind::Shutdown).unwrap();
    assert!(recorded < shutdown, "{recorded} {shutdown}");
    assert_eq!(fixture.offsets(), [0, 0, 0, 0]);
}

#[test]
fn a_pending_zero_dead_end_finishes_on_the_same_boot_and_every_later_run_stops_at_it_cleanly() {
    let fixture = zero_failure(true);
    let ended = fixture.resume();
    assert_zero_dead_end_finished(&fixture, &ended);
    assert_eq!(fixture.offsets(), [0, 0, 0, 0]);
    let mut seen = ended.journal.events().len();
    drop(ended);
    for _ in 0..2 {
        let again = fixture.resume();
        assert!(again.errors.is_empty(), "{:?}", again.errors);
        assert!(
            matches!(&again.stop, Some(Stop::DeadEnd { dead_end, .. })
                if dead_end.condition == DeadEndCondition::FailureAtZero),
            "{:?}",
            again.stop
        );
        let later = &again.journal.events()[seen..];
        let count = |kind| later.iter().filter(|event| event.kind() == kind).count();
        assert_eq!(count(EventKind::DeadEnd), 1);
        assert_eq!(count(EventKind::SmuIntent), 0);
        assert_eq!(later.last().unwrap().kind(), EventKind::Shutdown);
        assert_eq!(fixture.offsets(), [0, 0, 0, 0]);
        seen = again.journal.events().len();
    }
}

#[test]
fn a_pending_zero_dead_end_from_another_boot_finishes_without_refusal_and_leaves_zero() {
    let fixture = zero_failure(true);
    fixture.bench.sim.reboot();
    let ended = fixture.resume();
    assert_zero_dead_end_finished(&fixture, &ended);
}

#[test]
fn a_same_boot_resume_drains_the_pending_failure_before_it_restores() {
    let fixture = SameBoot::killed(true, |bench, session| {
        session.validated = Some(session.host.validate_smu().unwrap());
        bench
            .sim
            .script_trial(
                "0001",
                togi_sim::model::Script {
                    signal: Some(Signal::ComputationError),
                    at_s: 1.0,
                    core: core(2),
                    reset: None,
                    then_crash: false,
                },
            )
            .unwrap();
        let plan = search_plan(session, core(2), Regime::R1);
        run_plan(session, plan).unwrap();
        assert!(session.appender().tuner().drain().is_some());
    });
    let ended = fixture.resume();
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let (left, events) = ended.journal.events().split_at(fixture.left);
    assert_eq!(position(left, EventKind::Failure), None);
    assert!(position(left, EventKind::TrialEnd).is_some());
    let failure = events
        .iter()
        .find(|event| event.kind() == EventKind::Failure)
        .unwrap();
    let backoff = events
        .iter()
        .position(|event| {
            matches!(&event.payload, Payload::TunerDecision(decision)
                if decision.core == core(2) && event.cause.contains(&failure.seq))
        })
        .unwrap();
    let intent = position(events, EventKind::SmuIntent).unwrap();
    assert!(backoff < intent, "backoff {backoff}, intent {intent}");
    let held = togi_tuner::replay(ended.journal.events())
        .unwrap()
        .project()
        .cores
        .into_iter()
        .find(|held| held.core == core(2))
        .unwrap()
        .offset
        .get();
    assert!(held > -19, "{held}");
    assert_eq!(fixture.offsets(), [HELD[0], HELD[1], held, HELD[3]]);
}

fn assert_untouched(fixture: &SameBoot, events: &[Event]) {
    for event in events {
        assert!(
            !matches!(
                event.kind(),
                EventKind::BootSavedEntry | EventKind::Shutdown | EventKind::SmuIntent
            ),
            "{}: {}",
            event.kind().as_str(),
            event.msg
        );
    }
    assert_eq!(fixture.clears(), 0);
    assert_eq!(fixture.offsets(), LEFT);
}

#[test]
fn a_pending_boot_loop_survives_a_same_boot_preflight_that_fails_and_finishes_once_it_passes() {
    for check in ["watchdog", "root"] {
        let mut evidence = Vec::new();
        let mut original = None;
        let fixture = SameBoot::killed(false, |_, session| {
            for n in 0..3 {
                let crash = CrashDetected {
                    previous_boot: BootId::from(format!("prior-{n}")),
                    in_flight: None,
                    stray: true,
                    condition: None,
                    reset_reason: None,
                    reset_reason_raw: Vec::new(),
                    inconclusive: false,
                    pstore: None,
                };
                evidence.push(
                    session
                        .appender
                        .append(Payload::CrashDetected(crash), &[])
                        .unwrap(),
                );
            }
            let dead_end = DeadEnd {
                condition: DeadEndCondition::BootLoop,
                core: None,
                detail: "three crashes before application".into(),
                action: Some(DeadEndAction::ClearSavedEntryAndReboot),
            };
            let seq = session
                .appender
                .append(Payload::DeadEnd(dead_end.clone()), &evidence)
                .unwrap();
            original = Some((seq, dead_end));
        })
        .with_tuning_boot();
        let (original_seq, original) = original.unwrap();
        fixture
            .bench
            .sim
            .fail_check(check, "temporarily unavailable");
        let blocked = fixture.resume();
        assert!(blocked.errors.is_empty(), "{check}: {:?}", blocked.errors);
        assert_eq!(
            pending_dead_end(blocked.stop.as_ref()).map(|dead_end| dead_end.condition),
            Some(DeadEndCondition::Preflight),
            "{check}"
        );
        let after_original: Vec<Event> = blocked
            .journal
            .events()
            .iter()
            .filter(|event| event.seq > original_seq)
            .cloned()
            .collect();
        assert_eq!(position(&after_original, EventKind::DeadEnd), None);
        assert_untouched(&fixture, &after_original);
        drop(blocked);

        fixture.bench.sim.fail_check(check, "");
        let ended = fixture.resume();
        assert!(ended.errors.is_empty(), "{check}: {:?}", ended.errors);
        assert_eq!(
            ended.stop,
            Some(Stop::DeadEnd {
                dead_end: original.clone(),
                causes: evidence.clone(),
                reboot: true,
            }),
            "{check}"
        );
        assert_eq!(fixture.offsets(), HELD);
        assert_eq!(fixture.clears(), 1);
    }
}

#[test]
fn a_dead_end_a_same_boot_preflight_finds_waits_for_the_reconciliation_to_act() {
    let fixture = SameBoot::new(false).with_tuning_boot();
    fixture
        .bench
        .sim
        .fail_check("root", "temporarily unavailable");
    let blocked = fixture.resume();
    assert!(blocked.errors.is_empty(), "{:?}", blocked.errors);
    let Some(Stop::DeadEnd {
        dead_end: deferred, ..
    }) = blocked.stop.clone()
    else {
        panic!("{:?}", blocked.stop);
    };
    assert_eq!(deferred.condition, DeadEndCondition::Preflight);
    let events = fixture.since(&blocked);
    let recorded: Vec<&DeadEnd> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::DeadEnd(dead_end) => Some(dead_end),
            _ => None,
        })
        .collect();
    assert_eq!(recorded, [&deferred]);
    assert_untouched(&fixture, events);
    drop(blocked);

    fixture.bench.sim.fail_check("root", "");
    let ended = fixture.resume();
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(pending_dead_end(ended.stop.as_ref()), Some(&deferred));
    assert_eq!(fixture.offsets(), HELD);
    assert_eq!(fixture.clears(), 1);
}

/// A kernel whose log also holds `extra`, machine checks logged at the given times of this boot.
struct Logged {
    inner: Box<dyn Kernel>,
    extra: Vec<togi_machine::Mce>,
}

impl Kernel for Logged {
    fn mces(&mut self, boot: &BootId, since: Duration) -> Result<Vec<togi_machine::Mce>, Error> {
        self.inner.mces(boot, since)
    }

    fn read_mces(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<Vec<togi_machine::Mce>>> {
        let mut read = self.inner.read_mces(boot, after)?;
        read.mces.extend(self.extra.iter().cloned());
        Ok(read)
    }

    fn reset_reason(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        self.inner.reset_reason(boot)
    }

    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        self.inner.reset_reason_after(boot)
    }

    fn saved_pstore(&mut self, boot: &BootId) -> Result<Option<PstoreRecord>, Error> {
        self.inner.saved_pstore(boot)
    }
}

/// Trials that start nothing: the process dies once the setup of a trial is done.
struct DieAtStart(Box<dyn Trials>);

impl Trials for DieAtStart {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        self.0.sweep(cancel)
    }

    fn start(
        &mut self,
        _cancel: &CancellationToken,
        _spec: TrialSpec,
        _ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        Err(Partial {
            value: None,
            error: Error::Crashed,
        })
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        self.0.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = togi_machine::TrialConditions> + 'a> {
        self.0.samples(id)
    }
}

fn tick(sim: &Simulator, by: Duration) -> Duration {
    let mut clock = sim.seams().clock;
    clock.sleep(&CancellationToken::new(), by).unwrap();
    clock.monotonic()
}

fn machine_check(sim: &Simulator, monotonic: Duration) -> togi_machine::Mce {
    togi_machine::Mce {
        cpu: CpuId(0),
        core: Some(core(0)),
        bank: 1,
        bank_type: BankType::LoadStore,
        corrected: true,
        time: sim.now(),
        monotonic,
        lines: vec![format!("machine check at {monotonic:?}")],
    }
}

/// Runs a search trial on core 0 at -1 whose process dies as its setup ends, with a machine check logged just after
/// the target is written and another after the process died; `before_readback` kills the process before the target
/// is read back. Returns whether each machine check counts as between trials once the boot is reopened.
fn between_trials_after_setup(before_readback: bool) -> Vec<bool> {
    let mut bench = quiet(2, None);
    bench.config = Config::parse(r#"{"start_offsets":{"0":-1}}"#).unwrap();
    let mut input = bench.input();
    input.machine.trials = Box::new(DieAtStart(input.machine.trials));
    let mut session = match Session::open(input) {
        Ok(session) => session,
        Err(ended) => panic!("{:?}", ended.errors),
    };
    assert_eq!(session.start().unwrap(), Control::Continue);
    assert_eq!(session.sweep().unwrap(), Control::Continue);
    assert_eq!(session.begin().unwrap(), Control::Continue);
    tick(&bench.sim, Duration::from_secs(1));
    let logged = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&logged);
    let sim = bench.sim.clone();
    let written = Cell::new(false);
    bench.sim.interrupt_smu(move |operation: &SmuOperation| {
        match (operation.op, operation.phase) {
            (SmuOp::Set, HookPhase::After) if operation.offset == -1 => {
                let before = tick(&sim, Duration::from_secs(1));
                log.borrow_mut().push(machine_check(&sim, before));
                tick(&sim, Duration::from_secs(1));
                written.set(true);
            }
            (SmuOp::Read, HookPhase::Before) if written.get() && before_readback => {
                return Err(Error::Crashed);
            }
            _ => {}
        }
        Ok(())
    });
    let plan = search_plan(&session, core(0), Regime::R1);
    let outcome = run_plan(&mut session, plan);
    assert!(outcome.is_err(), "{outcome:?}");
    let after = tick(&bench.sim, Duration::from_secs(1));
    logged.borrow_mut().push(machine_check(&bench.sim, after));
    drop(session);
    bench.sim.interrupt_smu(|_: &SmuOperation| Ok(()));

    let mut input = bench.input();
    input.machine.kernel = Box::new(Logged {
        inner: input.machine.kernel,
        extra: logged.take(),
    });
    let mut session = Session::open(input).unwrap_or_else(|ended| panic!("{:?}", ended.errors));
    session.start().unwrap();
    session
        .appender()
        .journal()
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::Mce(mce) => Some(mce.between_trials),
            _ => None,
        })
        .collect()
}

#[test]
fn an_interrupted_setup_replays_only_the_matching_intent_write_readback_into_the_window() {
    assert_eq!(between_trials_after_setup(false), [true, false]);
    assert_eq!(between_trials_after_setup(true), [false, false]);
}

/// Trials that tell when the backend of a trial has started.
struct Marked {
    inner: Box<dyn Trials>,
    started: Rc<Cell<bool>>,
}

impl Trials for Marked {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let running = self.inner.start(cancel, spec, ready);
        self.started.set(true);
        running
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

fn one_cycle(bench: &Bench) -> crate::session::RunInput {
    let mut input = bench.input();
    input.policy.cycles = Some(std::num::NonZeroU32::MIN);
    input
}

fn shallow_start() -> Bench {
    let mut bench = quiet(2, None);
    bench.config = Config::parse(r#"{"start_offsets":{"0":-10,"1":-10}}"#).unwrap();
    bench
}

/// The journal write that records the first `trial.start`, which a run of the same machine reaches the same way.
fn first_trial_start_write() -> u64 {
    let bench = shallow_start();
    let ended = run(one_cycle(&bench));
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let at = position(ended.journal.events(), EventKind::TrialStart).unwrap();
    u64::try_from(at).unwrap() + 1
}

fn decisions(events: &[Event]) -> Vec<Payload> {
    events
        .iter()
        .filter(|event| {
            matches!(
                event.kind(),
                EventKind::Failure
                    | EventKind::TunerDecision
                    | EventKind::CorePhase
                    | EventKind::Combination
                    | EventKind::ProfileChange
                    | EventKind::HuntStart
                    | EventKind::HuntGroup
                    | EventKind::HuntEnd
                    | EventKind::DeepeningRound
                    | EventKind::TrialIntent
                    | EventKind::TrialEnd
            )
        })
        .map(|event| event.payload.clone())
        .collect()
}

/// A run whose journal fails at the record of its first `trial.start`, so the emergency zeroing answers it; the
/// machine goes down at the `at`th SMU access of that zeroing when given. After a power loss the next boot runs a
/// cycle. Returns the zeroing's SMU accesses and what the next boot decided.
fn zeroed_after_a_journal_failure(at: Option<usize>) -> (Vec<SmuOperation>, Vec<Payload>) {
    let bench = shallow_start();
    bench
        .mem
        .inject(Fault::before(Op::Write, first_trial_start_write()).named("events.jsonl"));
    let mut input = one_cycle(&bench);
    let started = Rc::new(Cell::new(false));
    input.machine.trials = Box::new(Marked {
        inner: input.machine.trials,
        started: Rc::clone(&started),
    });
    let accesses = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&accesses);
    let sim = bench.sim.clone();
    bench.sim.interrupt_smu(move |operation: &SmuOperation| {
        if !started.get() {
            return Ok(());
        }
        log.borrow_mut().push(*operation);
        if Some(log.borrow().len()) == at {
            sim.crash();
            return Err(Error::Crashed);
        }
        Ok(())
    });
    let ended = run(input);
    assert!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, SessionError::Journal(_))),
        "{:?}",
        ended.errors
    );
    assert!(ended.emergency.is_some());
    assert_eq!(
        position(ended.journal.events(), EventKind::TrialStart),
        None
    );
    drop(ended);
    bench.sim.interrupt_smu(|_: &SmuOperation| Ok(()));
    bench.mem.clear_faults();
    bench.sim.next_reset(togi_machine::ResetKind::PowerLoss);
    bench.sim.crash();
    bench.sim.reboot();
    let resumed = run(one_cycle(&bench));
    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    assert_eq!(resumed.stop, Some(Stop::Cycles));
    assert!(bench.sim.violations().is_empty());
    (accesses.take(), decisions(resumed.journal.events()))
}

#[test]
fn every_smu_access_window_of_the_emergency_zeroing_resumes_to_the_uninterrupted_decisions() {
    let (reference, want) = zeroed_after_a_journal_failure(None);
    assert!(
        reference
            .iter()
            .any(|access| access.op == SmuOp::SetAll && access.phase == HookPhase::After),
        "{reference:?}"
    );
    for (index, &access) in reference.iter().enumerate() {
        let (observed, got) = zeroed_after_a_journal_failure(Some(index + 1));
        assert_eq!(observed[index], access, "access #{}", index + 1);
        assert_eq!(got, want, "interrupted at access #{} {access:?}", index + 1);
    }
}

/// Cancels the run once it has recorded a profile change after a combination of this boot.
struct CancelAfterCommit {
    cancel: CancellationToken,
    combined: bool,
    line: String,
}

impl io::Write for CancelAfterCommit {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.line.push_str(&String::from_utf8_lossy(bytes));
        while let Some(end) = self.line.find('\n') {
            let line: String = self.line.drain(..=end).collect();
            match line.split_whitespace().nth(1) {
                Some("combination") => self.combined = true,
                Some("profile.change") if self.combined => self.cancel.cancel(),
                _ => {}
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn combining_bench() -> Bench {
    let mut bench = Bench::defined(&MachineDefinition {
        cores: 4,
        bios: Some(vec![-40, -40, 0, 0]),
        ranking: Some(vec![4, 3, 2, 1]),
        core: (0..4)
            .map(|id| CoreDef {
                id,
                alone: [-31; 5],
                together: [-31; 7],
                flat: 0.0,
                idle: None,
                workload: BTreeMap::new(),
            })
            .collect(),
        joint: vec![JointDef {
            members: [(0, -30), (1, -30)].into(),
            regimes: vec![Regime::R6],
            rate: Some(1e6),
            signal: Some(Signal::Crash),
            ..JointDef::default()
        }],
        model: ModelDef {
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    });
    bench.config = Config::parse(
        r#"{
            "candidate_solo_limits": {"0": -31, "1": -31, "2": -31, "3": -31},
            "checking": {"cycle": ["R6"]},
            "durations": {"short_trial_s": 1, "checking_trial_s": 1, "checking_all_core_s": 4},
            "evidence": {"rate": 0.95, "miss": 0.2}
        }"#,
    )
    .unwrap();
    bench
}

#[test]
fn a_restoration_after_a_combination_hunt_never_reaches_the_combination() {
    let bench = combining_bench();
    let mut finished = None;
    for _ in 0..40 {
        let mut input = bench.input();
        input.log = Some(Box::new(CancelAfterCommit {
            cancel: input.cancel.clone(),
            combined: false,
            line: String::new(),
        }));
        let ended = run(input);
        if ended.crashed() {
            bench.sim.reboot();
            continue;
        }
        finished = Some(ended);
        break;
    }
    let ended = finished.unwrap();
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(ended.stop, Some(Stop::Signal));
    let events = ended.journal.events();
    let combination = events
        .iter()
        .find_map(|event| match &event.payload {
            Payload::Combination(combination) => Some(combination.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        combination
            .members
            .iter()
            .map(|member| (member.core, member.offset.get()))
            .collect::<Vec<_>>(),
        [(core(0), -30), (core(1), -30)]
    );
    let committed = events
        .iter()
        .skip_while(|event| event.kind() != EventKind::Combination)
        .any(|event| event.kind() == EventKind::ProfileChange);
    assert!(committed);
    let mut registers = BIOS_OF_HUNT;
    let mut combined = false;
    for event in events {
        combined |= event.kind() == EventKind::Combination;
        let Payload::SmuReadback(readback) = &event.payload else {
            continue;
        };
        registers[readback.core.index()] = readback.offset.0;
        assert!(
            !(combined && registers[0] <= -30 && registers[1] <= -30),
            "readback #{} reaches the combination: {registers:?}",
            event.seq.get()
        );
    }
    assert_eq!(registers, [-30, -29, 0, 0]);
    let mut smu = bench.sim.seams().smu;
    let actual: Vec<i64> = (0..4).map(|n| smu.offset(core(n)).unwrap().0).collect();
    assert_eq!(actual, registers);
}

fn small_bench() -> Bench {
    let mut alone = [[-12; 5], [-13, -13, -13, -11, -13]];
    let mut limits = Vec::new();
    for (id, alone) in alone.iter_mut().enumerate() {
        let mut together = [0; 7];
        together[..5].copy_from_slice(alone);
        together[5] = alone.iter().copied().max().unwrap();
        together[6] = together[5];
        limits.push(CoreDef {
            id: i64::try_from(id).unwrap(),
            alone: *alone,
            together,
            flat: 0.0,
            idle: None,
            workload: BTreeMap::new(),
        });
    }
    Bench::defined(&MachineDefinition {
        cores: 2,
        bios: Some(vec![-10, -10]),
        core: limits,
        ..MachineDefinition::default()
    })
}

/// Lines of the run log as rendered.
struct Watch(Box<dyn FnMut(&str)>);

impl io::Write for Watch {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for line in String::from_utf8_lossy(bytes).lines() {
            (self.0)(line);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Trials whose next start takes the machine down once `armed`.
struct CrashAtStart {
    inner: Box<dyn Trials>,
    sim: Simulator,
    armed: Rc<Cell<bool>>,
}

impl Trials for CrashAtStart {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        if self.armed.take() {
            self.sim.crash();
            return Err(Partial {
                value: None,
                error: Error::Crashed,
            });
        }
        self.inner.start(cancel, spec, ready)
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

/// What the unlucky session of a hunt goes through, as the run log tells it: the machine goes down at the first
/// trial start once the first together profile is applied; the process is killed in the first nonzero write once a
/// hunt group is recorded; and once the resumed process has restored the offsets, the machine goes down at the readback
/// of its first nonzero write. With `crash_after_the_hunt` the machine also goes down at the first trial start once
/// the first hunt has ended and a together profile is applied. Boots run until one stops.
fn run_through_the_parked_deaths(bench: &Bench, crash_after_the_hunt: bool) -> Ended {
    let first_applied = Rc::new(Cell::new(true));
    let trial_crash = Rc::new(Cell::new(false));
    let kill_armed = Rc::new(Cell::new(false));
    let killed = Rc::new(Cell::new(false));
    let restored = Rc::new(Cell::new(false));
    let wrote = Rc::new(Cell::new(false));
    let grouped = Rc::new(Cell::new(false));
    let hunt_over = Rc::new(Cell::new(false));
    let hook_armed = Rc::clone(&kill_armed);
    let hook_killed = Rc::clone(&killed);
    bench.sim.interrupt_smu(move |operation: &SmuOperation| {
        if hook_armed.get()
            && operation.op == SmuOp::Set
            && operation.phase == HookPhase::After
            && operation.offset != 0
        {
            hook_armed.set(false);
            hook_killed.set(true);
            return Err(Error::Crashed);
        }
        Ok(())
    });
    for _ in 0..80 {
        let mut input = one_cycle(bench);
        input.machine.trials = Box::new(CrashAtStart {
            inner: input.machine.trials,
            sim: bench.sim.clone(),
            armed: Rc::clone(&trial_crash),
        });
        input.log = Some(Box::new({
            let (first_applied, trial_crash, kill_armed, restored, wrote, grouped, hunt_over) = (
                Rc::clone(&first_applied),
                Rc::clone(&trial_crash),
                Rc::clone(&kill_armed),
                Rc::clone(&restored),
                Rc::clone(&wrote),
                Rc::clone(&grouped),
                Rc::clone(&hunt_over),
            );
            let sim = bench.sim.clone();
            Watch(Box::new(move |line| {
                if line.starts_with("profile applied:") && first_applied.take() {
                    trial_crash.set(true);
                } else if line.starts_with("hunt 1 ended") {
                    hunt_over.set(crash_after_the_hunt);
                } else if line.starts_with("profile applied:") && hunt_over.take() {
                    trial_crash.set(true);
                } else if line.starts_with("hunt 1 group") && !grouped.replace(true) {
                    kill_armed.set(true);
                } else if line.starts_with("offsets restored") && grouped.get() {
                    restored.set(true);
                } else if restored.get() && line.starts_with("SMU wrote") && !line.ends_with("CO 0")
                {
                    wrote.set(true);
                } else if wrote.get() && line.starts_with("SMU core") {
                    wrote.set(false);
                    restored.set(false);
                    sim.crash();
                }
            }))
        }));
        let ended = run(input);
        if ended.crashed() {
            if !killed.replace(false) {
                bench.sim.reboot();
            }
            continue;
        }
        return ended;
    }
    panic!("no boot stopped");
}

#[test]
fn a_crash_in_the_reapplication_after_a_same_boot_restore_is_a_parked_crash_that_ends_the_hunt_on_its_core()
 {
    let bench = small_bench();
    let ended = run_through_the_parked_deaths(&bench, false);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(ended.stop, Some(Stop::Cycles));
    let events = ended.journal.events();
    let groups: Vec<usize> = (0..events.len())
        .filter(|&at| events[at].kind() == EventKind::HuntGroup)
        .collect();
    assert_eq!(groups.len(), 1, "the resume opened another group");
    let group = &events[groups[0]];
    assert!(
        events
            .iter()
            .any(|event| event.seq > group.seq && event.kind() == EventKind::ProfileRestored),
        "the resume restored nothing while the group was open"
    );
    let detected = events
        .iter()
        .find(|event| {
            matches!(&event.payload, Payload::CrashDetected(crash)
                if crash.condition == Some(Condition::Parked))
        })
        .unwrap();
    let failure = events
        .iter()
        .find(|event| event.kind() == EventKind::Failure && event.cause.contains(&detected.seq))
        .unwrap();
    let Payload::Failure(parked) = &failure.payload else {
        unreachable!()
    };
    assert_eq!(parked.signal, Signal::Crash);
    assert_eq!(parked.condition, Some(Condition::Parked));
    assert_eq!(parked.attribution, Attribution::Attributed);
    assert_eq!(parked.core, Some(core(1)));
    assert_eq!(
        parked.offset.map(togi_machine::TargetOffset::get),
        Some(-12)
    );
    let end = events
        .iter()
        .find(|event| event.kind() == EventKind::HuntEnd && event.cause.contains(&failure.seq))
        .unwrap();
    let Payload::HuntEnd(end) = &end.payload else {
        unreachable!()
    };
    assert_eq!(end.result, HuntOutcome::Direct);
    assert_eq!(end.cores.iter().collect::<Vec<_>>(), [core(1)]);
}

#[test]
fn a_hunt_groups_parked_condition_ends_with_the_hunt_so_the_next_together_application_crashes_as_together()
 {
    let bench = small_bench();
    let ended = run_through_the_parked_deaths(&bench, true);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(ended.stop, Some(Stop::Cycles));
    let events = ended.journal.events();
    let group = position(events, EventKind::HuntGroup).unwrap();
    let end = position(events, EventKind::HuntEnd).unwrap();
    let next_hunt = events
        .iter()
        .position(
            |event| matches!(&event.payload, Payload::HuntGroup(group) if group.hunt.get() == 2),
        )
        .unwrap();
    let applied: Vec<(usize, Condition)> = events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match &event.payload {
            Payload::ProfileApplied(applied) if applied.condition != Condition::Alone => {
                Some((at, applied.condition))
            }
            _ => None,
        })
        .filter(|&(at, _)| at > group && at < next_hunt)
        .collect();
    assert!(!applied.is_empty());
    for (at, condition) in applied {
        assert_eq!(
            condition == Condition::Parked,
            at < end,
            "application #{} as {condition:?}, the hunt ended at #{}",
            events[at].seq.get(),
            events[end].seq.get()
        );
    }
    let conditions: Vec<Option<Condition>> = events[group..next_hunt]
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::CrashDetected(crash) => Some(crash.condition),
            _ => None,
        })
        .collect();
    assert_eq!(
        conditions,
        [Some(Condition::Parked), Some(Condition::Together)]
    );
}
