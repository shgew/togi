//! Cutpoint sweeps over Ruleset 13: the journal of a scenario of nights and days is cut after each of its durable
//! events in turn, and the scenario resumed from the cut must reach what the uninterrupted scenario reached.
//!
//! Nights run through the public `run`. Days run through [`day`], a day host that, like `togi day`, derives
//! everything it writes from the journal and the policy state it folds, so a day cut after any record resumes where
//! it stopped.
//!
//! The swept window starts with the run that completes night 2's qualification and ends with the final day:
//! every durable event, without a stride. Projections and policy commitments compare all rendered values exactly,
//! naming identities by first mention. Sequence citations (including night IDs) also compare their target's kind
//! and identity-hidden policy payload in the whole journal, or kind alone for raw facts; missing targets are
//! distinct from local ones. Exposure anchors also hide the source boot because a resumed trial can run on a new
//! boot; directly compared exposures keep it exact. These bounded anchors do not recursively compare target
//! citations. Policy commitments compare as a multiset; trial identities compare only by first-mention aliasing.
//!
//! Machine crashes also cover every durable event. Power is cut before and after every actual SMU access.
//! Each resumed phase checks its registers and each crashed trial's duration and failure attribution. Between
//! night trials, decisions match an uninterrupted control with the reboot's elapsed time charged to its task;
//! crashes of open trials and day reboots introduce new failure or cadence evidence instead.

mod witnesses;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use test_case::test_case;
use togi_config::Config;
use togi_journal::fs::{AppendPoint, AppendTrigger, Fault, FileKind, Fs, MemFs, Op};
use togi_journal::{
    ApplicationId, ApplyCause, Attribution, CcdVector, DayApplied, DayBootStart, DayCheckpoint,
    DayCleanStop, DayOffsetsChanged, DayUncleanEnd, Event, Journal, Outcome, Payload,
    QualificationState, RateCell, Seq, Stamp, Tested, TrialId,
};
use togi_machine::{
    BootId, CancellationToken, Condition, EndEvidence, Error, Machine, ObservedOffset, PerCore,
    ResetReason, Signal,
};
use togi_sim::definition::CoreDef;
use togi_sim::faults::SmuOperation;
use togi_sim::{MachineDefinition, Model, Persisted, Simulator};
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::policy::{DayQuery, Params, Projection, State};
use togi_tuner::{Topology, unit::Units};

use super::compare::Names;
use crate::Stop;
use crate::testing::{Bench, build, simulated};

fn limits(id: i64, deepest: i64) -> CoreDef {
    CoreDef {
        id,
        alone: [deepest; 5],
        together: [deepest; 7],
        flat: 0.0,
        idle: None,
        workload: BTreeMap::new(),
    }
}

fn bench() -> Bench {
    Bench::defined(&MachineDefinition {
        cores: 4,
        core: (0..4).map(|id| limits(id, -50)).collect(),
        ..MachineDefinition::default()
    })
}

/// A night's milestone, given the night's events so far.
type Milestone = fn(&[Event]) -> bool;

fn qualified(night: &[Event]) -> bool {
    night.iter().any(|event| {
        matches!(&event.payload, Payload::NightQualification(record)
            if matches!(record.state, QualificationState::Complete { .. }))
    })
}

fn reproduced(night: &[Event]) -> bool {
    night
        .iter()
        .any(|event| matches!(event.payload, Payload::NightReproduction(_)))
}

/// Night 1 maps both CCDs; night 2 completes the qualification of a first promotion, which the day after applies.
/// A day at the promoted profile then ends unexplained, the next day retreats, and the night after reproduces
/// that end, closed by the last day.
const SCENARIO: &[Step] = &[
    Step::Night {
        until: qualified,
        hours: 12,
    },
    Step::Day { hours: 1 },
    Step::Night {
        until: qualified,
        hours: 12,
    },
    Step::Day { hours: 1 },
    Step::Crash { hours: 1 },
    Step::Day { hours: 1 },
    Step::Night {
        until: reproduced,
        hours: 12,
    },
    Step::Day { hours: 1 },
];

fn folded(events: &[Event]) -> State {
    let mut state = State::new(Params::SPEC);
    for event in events {
        state.fold(event).unwrap();
    }
    state
}

fn stamp(machine: &mut Machine<'_>) -> Stamp {
    Stamp {
        time: machine.clock.now(),
        mono: Some(machine.clock.monotonic().unwrap()),
    }
}

fn read_offsets(
    machine: &mut Machine<'_>,
    state: &State,
) -> Result<PerCore<ObservedOffset>, Error> {
    state
        .project()
        .units
        .iter()
        .flat_map(|unit| unit.cores.iter())
        .map(|core| machine.smu.offset(core).map(|offset| (core, offset)))
        .collect()
}

/// Only a selected power cut may make a simulated SMU access fail.
fn access<T>(result: Result<T, Error>, crashed: &mut bool) -> Option<T> {
    result
        .inspect_err(|error| {
            assert!(matches!(error, Error::Crashed), "{error:?}");
            *crashed = true;
        })
        .ok()
}

/// The day host's journal and the policy state folded from it.
struct DayJournal {
    journal: Journal,
    state: State,
    power: Option<(AppendTrigger, Simulator)>,
}

impl DayJournal {
    /// Appends `payload`, folding it; `None` when the append failed, which kills the day host.
    fn append(
        &mut self,
        machine: &mut Machine<'_>,
        payload: Payload,
        causes: &[Seq],
    ) -> Option<()> {
        let stamp = stamp(machine);
        let event = self.journal.append(stamp, payload, causes).ok()?.clone();
        self.state.fold(&event).unwrap();
        if let Some((trigger, sim)) = &self.power
            && trigger.lands(&event)
        {
            sim.power_loss();
            return None;
        }
        Some(())
    }

    fn events(&self) -> &[Event] {
        self.journal.events()
    }

    fn in_boot(&self, boot: &BootId) -> impl Iterator<Item = &Event> {
        self.events()
            .iter()
            .filter(move |event| event.boot == *boot)
    }

    /// The offsets `boot`'s observer last recorded.
    fn recorded_offsets(&self, boot: &BootId) -> Option<PerCore<ObservedOffset>> {
        self.in_boot(boot)
            .filter_map(|event| {
                if let Payload::DayBootStart(start) = &event.payload {
                    Some(start.offsets)
                } else if let Payload::DayOffsetsChanged(changed) = &event.payload {
                    Some(changed.offsets)
                } else {
                    None
                }
            })
            .last()
    }
}

/// A day boot of the day runtime on the machine's current boot: the observer's boot start and the unclean end of
/// the day boot before, the cold-boot invocation's owed commitments and its application, `hours` of use, and a
/// clean stop unless `clean` is false. Every record is written only when the journal lacks it, so a day host killed
/// after any of them and run again on the same boot finishes the day. False when an append failed.
#[expect(
    clippy::disallowed_methods,
    reason = "the day host applies the day profile to simulated registers only"
)]
fn day(
    bench: &Bench,
    trigger: Option<&AppendTrigger>,
    fate: Fate,
    hours: u64,
    clean: bool,
) -> RunEnd {
    let mut machine = bench.sim.seams();
    let boot = machine.host.boot_id().unwrap();
    let journal = simulated(&bench.mem)
        .open_at(boot.clone(), bench.sim.now())
        .unwrap();
    if let Some(trigger) = trigger {
        trigger.arm(&bench.mem, journal.events().len());
    }
    let state = folded(journal.events());
    let mut day = DayJournal {
        journal,
        state,
        power: trigger
            .filter(|_| matches!(fate, Fate::PowerLoss))
            .map(|trigger| (trigger.clone(), bench.sim.clone())),
    };
    let mut machine_crashed = false;
    let ran = (|| {
        if !day
            .in_boot(&boot)
            .any(|event| matches!(event.payload, Payload::DayBootStart(_)))
        {
            let offsets = access(read_offsets(&mut machine, &day.state), &mut machine_crashed)?;
            let bios = machine.host.bios_context().unwrap();
            let start = DayBootStart {
                build: build(),
                bios,
                offsets,
                watchdog: None,
            };
            day.append(&mut machine, start.into(), &[])?;
        }
        let crashed = day
            .events()
            .iter()
            .rev()
            .find(|event| matches!(event.payload, Payload::DayBootStart(_)) && event.boot != boot)
            .map(|event| event.boot.clone());
        let ended = |crashed: &BootId| {
            day.events().iter().any(|event| {
                matches!(event.payload, Payload::DayCleanStop(_)) && event.boot == *crashed
                    || matches!(&event.payload, Payload::DayUncleanEnd(end) if end.boot == *crashed)
            })
        };
        if let Some(crashed) = crashed.filter(|crashed| !ended(crashed)) {
            let last_checkpoint = day
                .in_boot(&crashed)
                .filter(|event| matches!(event.payload, Payload::DayCheckpoint(_)))
                .map(|event| event.seq)
                .last();
            let end = DayUncleanEnd {
                offsets: day.recorded_offsets(&crashed).unwrap(),
                boot: crashed,
                last_checkpoint,
                reset_reason: ResetReason {
                    kind: None,
                    raw: Vec::new(),
                    supported: false,
                },
                evidence: EndEvidence::unknown(),
            };
            day.append(&mut machine, end.into(), &[])?;
        }
        // The cold-boot invocation settles what the day query owes and ends with the boot's application.
        if !day
            .in_boot(&boot)
            .any(|event| matches!(event.payload, Payload::DayApplied(_)))
        {
            loop {
                let owed = day
                    .state
                    .day(&DayQuery {
                        boot: boot.clone(),
                        invocation: Invocation::ColdBoot {
                            pause: Pause::Running,
                        },
                        now: machine.clock.now(),
                    })
                    .unwrap();
                if owed.is_empty() {
                    break;
                }
                for decision in owed {
                    day.append(&mut machine, decision.payload.into(), &decision.causes)?;
                }
            }
            let step = day.events().iter().rev().find_map(|event| {
                if let Payload::DayStep(step) = &event.payload
                    && step.cold_boot == boot
                {
                    Some((step.decision, step.proposed.clone()))
                } else {
                    None
                }
            });
            let (cause, target) = if let Some((decision, proposed)) = step {
                (ApplyCause::Step { decision }, proposed)
            } else {
                let applied = day.state.project().applied.unwrap().to_ccd().unwrap();
                let cores = machine.host.topology().unwrap();
                let target = CcdVector::expand(applied, &cores).unwrap();
                (ApplyCause::Reapplication {}, target)
            };
            for (core, offset) in target.cores().iter() {
                access(machine.smu.set_offset(core, offset), &mut machine_crashed)?;
            }
            let readbacks = access(read_offsets(&mut machine, &day.state), &mut machine_crashed)?;
            let applications = day
                .events()
                .iter()
                .filter(|event| matches!(event.payload, Payload::DayApplied(_)))
                .count();
            let applied = DayApplied {
                application: ApplicationId(u64::try_from(applications).unwrap()),
                cause,
                target,
                readbacks,
                transactions: Vec::new(),
                boot: boot.clone(),
                mono_ns: u64::try_from(machine.clock.monotonic().unwrap().as_nanos()).unwrap(),
            };
            day.append(&mut machine, applied.into(), &[])?;
        }
        let readbacks = access(read_offsets(&mut machine, &day.state), &mut machine_crashed)?;
        if day.recorded_offsets(&boot) != Some(readbacks) {
            let changed = DayOffsetsChanged { offsets: readbacks };
            day.append(&mut machine, changed.into(), &[])?;
        }
        if !day
            .in_boot(&boot)
            .any(|event| matches!(event.payload, Payload::DayCheckpoint(_)))
        {
            machine
                .clock
                .sleep(&CancellationToken::new(), Duration::from_secs(hours * 3600))
                .unwrap();
            let checkpoint = DayCheckpoint {
                applied_secs: hours * 3600,
                load_mix: togi_journal::LoadMix::default(),
                watchdog: None,
            };
            day.append(&mut machine, checkpoint.into(), &[])?;
        }
        if clean
            && !day
                .in_boot(&boot)
                .any(|event| matches!(event.payload, Payload::DayCleanStop(_)))
        {
            day.append(&mut machine, DayCleanStop {}.into(), &[])?;
        }
        Some(())
    })()
    .is_some();
    let fired = trigger.is_some_and(|trigger| trigger.disarm(&bench.mem));
    if ran {
        assert!(!fired, "the day continued after its cut");
        let events = day.events().to_vec();
        drop(day.journal.close().unwrap());
        RunEnd::Stopped(None, events)
    } else if fired {
        RunEnd::Cut(machine_crashed || matches!(fate, Fate::PowerLoss))
    } else {
        assert!(machine_crashed, "a day append failed without the cut");
        RunEnd::Crashed
    }
}

/// A run of the session on the current boot that the operator ends once `trials` trials ended uninterrupted in the
/// journal after its first `base` events.
fn segment(
    bench: &Bench,
    trigger: Option<&AppendTrigger>,
    fate: Fate,
    base: usize,
    trials: usize,
) -> RunEnd {
    let mut input = bench.input();
    let ended_whole =
        |event: &Event| matches!(&event.payload, Payload::TrialEnd(end) if !end.interrupted);
    let mut whole = input.journal.events()[base..]
        .iter()
        .filter(|event| ended_whole(event))
        .count();
    let cancel = input.cancel.clone();
    if whole >= trials {
        cancel.cancel();
    }
    let power = trigger
        .filter(|_| matches!(fate, Fate::PowerLoss | Fate::Paused))
        .cloned();
    let sim = bench.sim.clone();
    let mem = bench.mem.clone();
    input.on_append = Some(Box::new(move |event| {
        if ended_whole(event) {
            whole += 1;
            if whole >= trials {
                cancel.cancel();
            }
        }
        if power.as_ref().is_some_and(|trigger| trigger.lands(event)) {
            if matches!(fate, Fate::Paused) {
                sim.seams()
                    .clock
                    .sleep(&CancellationToken::new(), togi_sim::REBOOT_TIME)
                    .unwrap();
            } else {
                sim.power_loss();
                mem.inject(Fault::before(Op::Write, 1).named("events.jsonl"));
            }
        }
    }));
    if let Some(trigger) = trigger {
        trigger.arm(&bench.mem, input.journal.events().len());
    }
    let ended = crate::run(input);
    if trigger.is_some_and(|trigger| trigger.disarm(&bench.mem)) && !matches!(fate, Fate::Paused) {
        bench.mem.clear_faults();
        assert!(
            ended.capsule.is_none() || matches!(fate, Fate::PowerLoss),
            "the cut event failed, yet the run went on"
        );
        return RunEnd::Cut(
            matches!(fate, Fate::PowerLoss)
                || ended.errors.iter().any(crate::SessionError::is_crash),
        );
    }
    if ended.errors.iter().any(crate::SessionError::is_crash) {
        return RunEnd::Crashed;
    }
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let stop = ended.stop;
    let events = ended.journal.events().to_vec();
    drop(ended.journal.close().unwrap());
    RunEnd::Stopped(stop, events)
}

enum RunEnd {
    Stopped(Option<Stop>, Vec<Event>),
    /// The machine crashed under a trial; it reboots and the run starts again.
    Crashed,
    /// The cut interrupted the run; true when the machine had crashed as well.
    Cut(bool),
}

/// How the cut befalls the machine.
#[derive(Clone, Copy, Debug)]
enum Fate {
    /// The process dies and starts again on the same boot.
    Killed,
    /// The machine crashes and boots again.
    Crashed,
    /// A physical power loss, before any cleanup can run.
    PowerLoss,
    /// The uninterrupted control spends the same time as the reboot at this point, without losing evidence.
    Paused,
}

/// One step of a scenario as it is played: a reboot, a run, or a day boot.
#[derive(Clone, Copy, Debug)]
enum Phase {
    Boot,
    /// A run the operator ends after `trials` uninterrupted trials.
    Run {
        trials: usize,
    },
    Day {
        hours: u64,
    },
    /// A day boot the CPU ends after `hours`; the machine boots again.
    Crash {
        hours: u64,
    },
}

const BOOTS: usize = 50;

/// Plays `phase` on `bench`, whose journal holds `base` events, with the journal cut after its `cut`th event if one
/// is given and `fate` befalling the machine there. Keeps the last run's already decoded journal: reopening it
/// for each assertion would repeatedly decode the entire scenario prefix at every cut.
fn play(
    bench: &Bench,
    phase: Phase,
    base: usize,
    cut: Option<(usize, Fate)>,
) -> (Option<Stop>, Vec<Event>) {
    let trigger = cut.map(|(after, fate)| {
        AppendTrigger::new(
            after,
            if matches!(fate, Fate::PowerLoss | Fate::Paused) {
                AppendPoint::Landed
            } else {
                AppendPoint::BeforeSync
            },
        )
    });
    let fate = cut.map_or(Fate::Killed, |(_, fate)| fate);
    for _ in 0..BOOTS {
        let ended = match phase {
            Phase::Boot => {
                bench.sim.reboot().unwrap();
                return (None, durable(bench));
            }
            Phase::Run { trials } => segment(bench, trigger.as_ref(), fate, base, trials),
            Phase::Day { hours } => day(bench, trigger.as_ref(), fate, hours, true),
            Phase::Crash { hours } => day(bench, trigger.as_ref(), fate, hours, false),
        };
        match ended {
            RunEnd::Stopped(stop, events) => {
                if matches!(phase, Phase::Crash { .. }) {
                    bench.sim.crash();
                    bench.sim.reboot().unwrap();
                }
                return (stop, events);
            }
            RunEnd::Crashed => bench.sim.reboot().unwrap(),
            RunEnd::Cut(crashed) => {
                assert_eq!(durable_lines(bench), cut.unwrap().0);
                if crashed || matches!(fate, Fate::Crashed) {
                    if !crashed {
                        bench.sim.crash();
                    }
                    bench.sim.reboot().unwrap();
                }
            }
        }
    }
    panic!("{phase:?} did not finish in {BOOTS} boots");
}

/// The complete lines of the journal on disk, counted without decoding them.
fn durable_lines(bench: &Bench) -> usize {
    let bytes = bench
        .mem
        .contents("/state/events.jsonl")
        .unwrap_or_default();
    bytes
        .split_inclusive(|&byte| byte == b'\n')
        .filter(|line| line.ends_with(b"\n"))
        .count()
}

fn durable(bench: &Bench) -> Vec<Event> {
    let boot = bench.sim.seams().host.boot_id().unwrap();
    simulated(&bench.mem)
        .open_at(boot, bench.sim.now())
        .unwrap()
        .events()
        .to_vec()
}

/// What a bench keeps between phases: the machine and every file.
struct Snapshot {
    machine: Persisted,
    files: Vec<(PathBuf, Option<Vec<u8>>)>,
}

impl Snapshot {
    fn of(bench: &Bench) -> Self {
        let files = bench
            .mem
            .entries()
            .into_iter()
            .map(|(path, kind)| match kind {
                FileKind::Dir => (path, None),
                FileKind::File => {
                    let bytes = bench.mem.contents(&path).unwrap();
                    (path, Some(bytes))
                }
                FileKind::Symlink | FileKind::Other => panic!("{path:?} is a {kind:?}"),
            })
            .collect();
        Self {
            machine: bench.sim.persisted(),
            files,
        }
    }

    /// A bench of `model` in this state, sharing nothing with the one it was taken of.
    fn bench(&self, model: &Model) -> Bench {
        let mem = MemFs::new();
        for (path, bytes) in &self.files {
            match bytes {
                Some(bytes) => mem.put(path, bytes.clone()),
                None => {
                    if mem.kind(path).unwrap().is_none() {
                        mem.create_dir(path).unwrap();
                    }
                }
            }
        }
        Bench {
            sim: Simulator::restore(model.clone(), None, self.machine.clone()).unwrap(),
            mem,
            config: Config::default(),
            defects: &[],
        }
    }
}

/// One step of a scenario as it is written: a night the operator ends at its milestone or after `hours`, or a day
/// boot, clean or ended by a crash.
#[derive(Clone, Copy, Debug)]
enum Step {
    Night { until: Milestone, hours: i64 },
    Day { hours: u64 },
    Crash { hours: u64 },
}

/// Uninterrupted trials per run of a night.
const SEGMENT: usize = 20;

/// A phase of the uninterrupted scenario: the state before it, the journal length before it, and the journal after.
struct Played {
    phase: Phase,
    before: Snapshot,
    start: usize,
    events: Vec<Event>,
    accesses: Vec<SmuOperation>,
}

impl Played {
    /// This phase's own events.
    fn own(&self) -> &[Event] {
        &self.events[self.start..]
    }
}

fn step(bench: &Bench, played: &mut Vec<Played>, phase: Phase) -> Option<Stop> {
    let before = Snapshot::of(bench);
    let start = durable_lines(bench);
    let accesses = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&accesses);
    bench.sim.interrupt_smu(move |operation: &SmuOperation| {
        seen.borrow_mut().push(*operation);
        Ok(())
    });
    let (stop, events) = play(bench, phase, start, None);
    bench.sim.interrupt_smu(|_: &SmuOperation| Ok(()));
    assert_settled(bench, phase, &events);
    played.push(Played {
        phase,
        before,
        start,
        events,
        accesses: accesses.take(),
    });
    stop
}

/// Plays `steps` uninterrupted on `bench`, a reboot between steps, each night in runs of [`SEGMENT`] trials.
fn reference(bench: &Bench, steps: &[Step]) -> Vec<Played> {
    let mut played = Vec::new();
    let mut crashed = false;
    for (at, &next) in steps.iter().enumerate() {
        if at > 0 && !crashed {
            step(bench, &mut played, Phase::Boot);
        }
        crashed = false;
        match next {
            Step::Night { until, hours } => {
                let start = durable_lines(bench);
                let deadline = bench.sim.now() + jiff::SignedDuration::from_hours(hours);
                loop {
                    let stop = step(bench, &mut played, Phase::Run { trials: SEGMENT });
                    let night = &played.last().unwrap().events[start..];
                    if stop != Some(Stop::Signal) || until(night) || bench.sim.now() >= deadline {
                        break;
                    }
                }
            }
            Step::Day { hours } => {
                step(bench, &mut played, Phase::Day { hours });
            }
            Step::Crash { hours } => {
                step(bench, &mut played, Phase::Crash { hours });
                crashed = true;
            }
        }
    }
    played
}

/// The scenario played uninterrupted, and the first phase the sweeps cut: the run that completes night 2's
/// qualification. The phases before it, night 1 included, are only the fixture the swept phases start from.
fn scenario() -> (Model, Vec<Played>, usize) {
    let bench = bench();
    let played = reference(&bench, SCENARIO);
    assert!(
        bench.sim.violations().is_empty(),
        "{:?}",
        bench.sim.violations()
    );
    let from = played
        .iter()
        .position(|phase| qualified(phase.own()))
        .expect("no night completed a qualification");
    let last = played.last().unwrap();
    assert!(
        reproduced(&last.events[played[from].start..]),
        "the swept phases reproduce nothing"
    );
    (bench.sim.model(), played, from)
}

/// Every cut of the swept phases: the phase and the durable events the cut leaves.
fn cuts(played: &[Played], from: usize) -> Vec<(usize, usize)> {
    played
        .iter()
        .enumerate()
        .skip(from)
        .flat_map(|(at, phase)| {
            (phase.start + 1..=phase.events.len()).map(move |after| (at, after))
        })
        .collect()
}

/// CI runs the sweep tests sequentially; each sweep uses this many cut workers, independent of the host.
const CUT_WORKERS: usize = 2;

/// Runs `check` on every cut in parallel, and fails with every cut it failed on.
fn each(cuts: &[(usize, usize)], check: impl Fn(usize, usize) + Sync) {
    let next = AtomicUsize::new(0);
    let failed = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..CUT_WORKERS {
            scope.spawn(|| {
                while let Some(&(phase, after)) = cuts.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let checked =
                        std::panic::catch_unwind(AssertUnwindSafe(|| check(phase, after)));
                    if let Err(panic) = checked {
                        let message = panic
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| {
                                panic
                                    .downcast_ref::<&str>()
                                    .map(|message| (*message).to_owned())
                            })
                            .unwrap_or_default();
                        failed.lock().unwrap().push((after, message));
                    }
                }
            });
        }
    });
    let mut failed = failed.into_inner().unwrap();
    failed.sort();
    assert!(
        failed.is_empty(),
        "{} of {} cuts failed:\n{}",
        failed.len(),
        cuts.len(),
        failed
            .iter()
            .map(|(after, message)| format!("cut after event {after}: {message}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// A night restores CO 0, a day holds its applied profile, and a reboot clears the registers.
fn assert_settled(bench: &Bench, phase: Phase, events: &[Event]) {
    let mut machine = bench.sim.seams();
    let state = folded(events);
    let offsets = read_offsets(&mut machine, &state).unwrap();
    let target = if matches!(phase, Phase::Day { .. }) {
        let applied = state.project().applied.unwrap();
        let topology = Topology::new(&machine.host.topology().unwrap()).unwrap();
        Some(
            applied
                .expand(&Units::for_profile(&topology, &applied))
                .unwrap(),
        )
    } else {
        None
    };
    for (core, offset) in offsets.iter() {
        let expected = target.as_ref().map_or(ObservedOffset(0), |target| {
            target.get(core).unwrap().observed()
        });
        assert_eq!(offset, expected, "unsettled core {core} after {phase:?}");
    }
    assert!(
        bench.sim.violations().is_empty(),
        "{:?}",
        bench.sim.violations()
    );
}

/// A new boot must account for the lost boot and for its trial, not merely leave a foldable journal.
fn assert_crash_accounted(events: &[Event], after: usize, boot: &BootId) {
    let prefix = &events[..after];
    if !prefix.iter().any(|event| event.boot == *boot)
        || prefix
            .iter()
            .any(|event| event.boot == *boot && matches!(event.payload, Payload::DayCleanStop(_)))
        || prefix
            .iter()
            .rev()
            .find(|event| {
                event.boot == *boot
                    && !matches!(
                        event.payload,
                        Payload::SessionWarning(_) | Payload::BootLeaveReason(_)
                    )
            })
            .is_some_and(|event| matches!(event.payload, Payload::Shutdown(_)))
    {
        return;
    }
    if prefix
        .iter()
        .any(|event| event.boot == *boot && matches!(event.payload, Payload::DayBootStart(_)))
    {
        assert!(
            events[after..].iter().any(|event| {
                matches!(&event.payload, Payload::DayUncleanEnd(end) if end.boot == *boot)
            }),
            "no day.unclean_end for {boot}"
        );
        return;
    }
    let detected = events[after..].iter().find(|event| {
        matches!(&event.payload, Payload::CrashDetected(crash) if crash.previous_boot == *boot)
    }).unwrap_or_else(|| panic!("no crash.detected for {boot} after #{after}"));
    let Some((at, intent)) = prefix.iter().enumerate().rev().find_map(|(at, event)| {
        if event.boot == *boot
            && let Payload::TrialIntent(intent) = &event.payload
        {
            Some((at, intent))
        } else {
            None
        }
    }) else {
        return;
    };
    let durable_end = prefix[at..].iter().find_map(|event| {
        if let Payload::TrialEnd(end) = &event.payload
            && end.trial == intent.trial
        {
            Some(end)
        } else {
            None
        }
    });
    let ends: Vec<_> = events[at..]
        .iter()
        .filter_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload
                && end.trial == intent.trial
            {
                Some((event, end))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        ends.len(),
        1,
        "trial {} closed {} times",
        intent.trial,
        ends.len()
    );
    let (closed, end) = ends[0];
    if durable_end.is_none() {
        let start = prefix[at..].iter().find(|event| {
            matches!(&event.payload, Payload::TrialStart(start) if start.trial == intent.trial)
        }).and_then(|event| event.mono);
        let last = prefix[at..]
            .iter()
            .rev()
            .find(|event| {
                matches!(
                    event.payload,
                    Payload::TrialStart(_)
                        | Payload::TrialProgress(_)
                        | Payload::TrialSignal(_)
                        | Payload::TrialSample(_)
                )
            })
            .and_then(|event| event.mono);
        let duration = start
            .zip(last)
            .map_or(Duration::ZERO, |(start, last)| last.saturating_sub(start));
        assert_eq!(
            u64::from(end.duration_s),
            duration.as_secs(),
            "trial {}",
            intent.trial
        );
        assert_eq!(end.outcome, Outcome::Failure, "trial {}", intent.trial);
        assert!(
            closed.cause.contains(&detected.seq),
            "trial end does not cite its crash"
        );
    }
    let failures: Vec<_> = events[at..]
        .iter()
        .filter_map(|event| {
            if let Payload::Failure(failure) = &event.payload
                && failure.trial.as_ref() == Some(&intent.trial)
            {
                Some(failure)
            } else {
                None
            }
        })
        .collect();
    if end.outcome == Outcome::Failure {
        assert_eq!(
            failures.len(),
            1,
            "failure attribution for trial {}",
            intent.trial
        );
        let failure = failures[0];
        assert_eq!(failure.condition, Some(intent.condition));
        assert_eq!(failure.profile.as_ref(), Some(&intent.profile));
        assert_eq!(failure.core, end.core);
        assert_eq!(failure.signal, end.signal.unwrap_or(Signal::Crash));
        assert_eq!(
            failure.attribution,
            if end.core.is_some() {
                Attribution::Attributed
            } else {
                Attribution::Unattributed
            }
        );
        assert_eq!(
            failure.offset,
            match intent.tested {
                Some(Tested::Isolated { offset, .. }) => Some(offset),
                Some(Tested::Exact {}) | None => None,
            }
        );
        if intent.condition == Condition::Alone {
            assert_eq!(failure.core, intent.core);
            assert_eq!(failure.offset, intent.offset);
        }
    } else {
        assert!(failures.is_empty(), "a successful trial acquired a failure");
    }
}

/// Plays `played`'s phase from its start with the journal cut after its `after`th event and `fate` befalling the
/// machine there, and checks that the journal left is resumable. Returns the journal at the phase's end.
fn resume(played: &Played, model: &Model, after: usize, fate: Fate) -> Vec<Event> {
    let bench = played.before.bench(model);
    let (_, mut events) = play(&bench, played.phase, played.start, Some((after, fate)));
    if !matches!(fate, Fate::Killed) {
        events = drain_failure(&bench, played.phase, events);
    }
    let facts = togi_journal::State::replay(&events).unwrap();
    assert!(facts.open.is_empty(), "left open: {:?}", facts.open);
    assert_settled(&bench, played.phase, &events);
    if !matches!(fate, Fate::Killed) {
        assert_crash_accounted(&events, after, &events[after - 1].boot);
    }
    events
}

/// The same phase runner, losing power before or after the selected actual SMU access.
fn resume_at_smu(played: &Played, model: &Model, at: usize) -> Vec<Event> {
    let bench = played.before.bench(model);
    let boot = bench.sim.seams().host.boot_id().unwrap();
    let sim = bench.sim.clone();
    let mem = bench.mem.clone();
    let cut = Rc::new(Cell::new(None));
    let fired = Rc::clone(&cut);
    let mut accesses = 0;
    let expected = played.accesses[at - 1];
    bench.sim.interrupt_smu(move |operation: &SmuOperation| {
        accesses += 1;
        if accesses == at {
            assert_eq!(*operation, expected, "cut another SMU access");
            let bytes = mem.contents("/state/events.jsonl").unwrap();
            fired.set(Some(
                bytes
                    .split_inclusive(|&byte| byte == b'\n')
                    .filter(|line| line.ends_with(b"\n"))
                    .count(),
            ));
            sim.power_loss();
            return Err(Error::Crashed);
        }
        Ok(())
    });
    let (_, events) = play(&bench, played.phase, played.start, None);
    bench.sim.interrupt_smu(|_: &SmuOperation| Ok(()));
    let after = cut.get().expect("the selected SMU access never ran");
    let events = drain_failure(&bench, played.phase, events);
    assert!(
        togi_journal::State::replay(&events)
            .unwrap()
            .open
            .is_empty()
    );
    assert_settled(&bench, played.phase, &events);
    assert_crash_accounted(&events, after, &boot);
    assert_unchanged_decisions(played, model, &events, after, &boot, Some(at));
    events
}

/// A crash of an open trial is new failure evidence; a day reboot is new cadence/end evidence. Neither may
/// be compared to a run without that evidence. Between night trials, however, all policy decisions must agree.
fn assert_unchanged_decisions(
    played: &Played,
    model: &Model,
    events: &[Event],
    after: usize,
    boot: &BootId,
    smu: Option<usize>,
) {
    if !matches!(played.phase, Phase::Run { .. }) {
        return;
    }
    let prefix = &events[..after];
    let open = prefix
        .iter()
        .rev()
        .find_map(|event| {
            if event.boot != *boot {
                None
            } else if matches!(event.payload, Payload::TrialEnd(_)) {
                Some(false)
            } else if matches!(event.payload, Payload::TrialIntent(_)) {
                Some(true)
            } else {
                None
            }
        })
        .unwrap_or(false);
    if !open {
        // Recovery downtime is charged to the task. Keep it in the uninterrupted control too: comparing to
        // a faster run would demand identical scheduling from different wall-budget evidence.
        let control = played.before.bench(model);
        let append = if let Some(at) = smu {
            let sim = control.sim.clone();
            let mut accesses = 0;
            control.sim.interrupt_smu(move |_: &SmuOperation| {
                accesses += 1;
                if accesses == at {
                    sim.seams()
                        .clock
                        .sleep(&CancellationToken::new(), togi_sim::REBOOT_TIME)?;
                }
                Ok(())
            });
            None
        } else {
            Some((after, Fate::Paused))
        };
        let (_, reference) = play(&control, played.phase, played.start, append);
        let got = commitments(events, played.start, false);
        let want = commitments(&reference, played.start, false);
        assert_lines_equal(&got, &want, "decisions");
    }
}

/// Report the first differing line rather than printing the entire scenario's evidence.
fn assert_lines_equal(got: &[String], want: &[String], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: count");
    for (at, (got, want)) in got.iter().zip(want).enumerate() {
        assert_eq!(got, want, "{what}[{at}]");
    }
}

/// Cancellation at the phase's last trial can leave its policy commitments for the next invocation. Give a
/// recovered failure that invocation before checking its attribution, rather than stopping before it is owed.
fn drain_failure(bench: &Bench, phase: Phase, events: Vec<Event>) -> Vec<Event> {
    if !matches!(phase, Phase::Run { .. }) {
        return events;
    }
    let pending = events.iter().any(|event| {
        if let Payload::TrialEnd(end) = &event.payload
            && end.outcome == Outcome::Failure
        {
            !events.iter().any(|event| {
                matches!(&event.payload, Payload::Failure(failure)
                    if failure.trial.as_ref() == Some(&end.trial))
            })
        } else {
            false
        }
    });
    if pending {
        play(bench, Phase::Run { trials: 1 }, events.len(), None).1
    } else {
        events
    }
}

/// Every projection field, with citation aliasing and the whole journal's bounded target anchors.
fn compared_projection(events: &[Event], projection: &Projection) -> Vec<String> {
    Names::numbered(events).projection(projection)
}

/// The tuner's projection of `events`, as it is compared.
fn projection(events: &[Event]) -> Vec<String> {
    compared_projection(events, &folded(events).project())
}

/// The policy commitments from `from` onward, without observations of interrupted trials, which only the resumed
/// journal has. An observation may commit an unchanged cap before a retried trial rather than after it, so
/// commitments are sorted by identity-hidden shape before naming citations. Anchors use the whole journal,
/// including records before `from`, so a suffix's citations do not accidentally become outside-journal names.
fn commitments(events: &[Event], from: usize, observations: bool) -> Vec<String> {
    let compared = &events[from..];
    let interrupted: Vec<_> = compared
        .iter()
        .filter_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload
                && end.interrupted
            {
                Some(&end.trial)
            } else {
                None
            }
        })
        .collect();
    let mut chosen: Vec<_> = compared
        .iter()
        .filter(|event| {
            matches!(
                event.payload,
                Payload::DeadEnd(_)
                    | Payload::CcdEdge(_)
                    | Payload::CcdCap(_)
                    | Payload::NightPlan(_)
                    | Payload::NightQualification(_)
                    | Payload::NightReproduction(_)
                    | Payload::DayStep(_)
                    | Payload::DayApplied(_)
                    | Payload::DayEvent(_)
                    | Payload::DayVeto(_)
                    | Payload::DayHold(_)
                    | Payload::DayFreeze(_)
                    | Payload::DayInterval(_)
            ) || observations
                && matches!(&event.payload, Payload::ExposureObserved(observed)
                if observed.source.trial.as_ref().is_none_or(|trial| !interrupted.contains(&trial)))
        })
        .collect();
    let mut shapes = Names::hidden();
    chosen.sort_by_cached_key(|event| {
        format!(
            "{} {}",
            event.kind().as_str(),
            shapes.payload(&event.payload)
        )
    });
    let mut names = Names::numbered(events);
    chosen
        .into_iter()
        .map(|event| {
            format!(
                "{} {}",
                event.kind().as_str(),
                names.payload(&event.payload)
            )
        })
        .collect()
}

/// The kinds the kill sweep must cut after at least once.
const KINDS: &[&str] = &[
    "task.start",
    "night.plan",
    "exposure.observed",
    "ccd.edge",
    "ccd.cap",
    "night.qualification",
    "night.reproduction",
    "day.step",
    "day.applied",
];

/// The fewest swept phases that cut something: after the run that completes night 2's qualification, the scenario
/// holds that run, three days, the crashed day boot and night 3's run.
const MIN_SWEPT_PHASES: usize = 6;

/// What the sweep window must hold for a sweep over it to mean anything, however many events a legitimate change
/// adds to it: a cut after every kind of [`KINDS`], one inside a trial, and the scenario's swept phases.
fn assert_window(played: &[Played], from: usize, cuts: &[(usize, usize)]) {
    let cut: BTreeSet<_> = cuts
        .iter()
        .map(|&(phase, after)| played[phase].events[after - 1].kind().as_str())
        .collect();
    let missed: Vec<_> = KINDS.iter().filter(|kind| !cut.contains(*kind)).collect();
    assert!(missed.is_empty(), "no cut after {missed:?}");
    let trials = trial_cuts(played, from);
    assert!(
        !trials.is_empty() && trials.iter().all(|cut| cuts.contains(cut)),
        "the swept phases cut no trial"
    );
    let phases: BTreeSet<_> = cuts.iter().map(|&(phase, _)| phase).collect();
    assert!(
        phases.len() >= MIN_SWEPT_PHASES,
        "only {} swept phases hold a cut, fewer than {MIN_SWEPT_PHASES}",
        phases.len()
    );
}

#[test]
fn killing_the_scenario_after_every_durable_event_resumes_to_the_same_projection_and_commitments() {
    let (model, played, from) = scenario();
    let cuts = cuts(&played, from);
    assert_window(&played, from, &cuts);
    let wanted: Vec<_> = played
        .iter()
        .map(|phase| projection(&phase.events))
        .collect();
    each(&cuts, |phase, after| {
        let reference = &played[phase];
        let events = resume(reference, &model, after, Fate::Killed);
        assert_eq!(&events[..after], &reference.events[..after]);
        assert_eq!(projection(&events), wanted[phase]);
        assert_eq!(
            commitments(&events, after, true),
            commitments(&reference.events, after, true)
        );
    });
}

/// The events of each trial on its own boot, from its intent through its end: the cuts where a crash leaves a trial
/// to recover.
fn trial_cuts(played: &[Played], from: usize) -> Vec<(usize, usize)> {
    let mut cuts = Vec::new();
    for (at, phase) in played.iter().enumerate().skip(from) {
        let mut open: Option<(&BootId, &TrialId)> = None;
        for (index, event) in phase.own().iter().enumerate() {
            if let Payload::TrialIntent(intent) = &event.payload {
                open = Some((&event.boot, &intent.trial));
            }
            if let Some((boot, trial)) = open
                && event.boot == *boot
            {
                cuts.push((at, phase.start + index + 1));
                if matches!(&event.payload, Payload::TrialEnd(end) if end.trial == *trial) {
                    open = None;
                }
            }
        }
    }
    cuts
}

#[test]
fn crashes_at_every_durable_event_and_smu_access_account_for_trials_and_preserve_decisions() {
    let (model, played, from) = scenario();
    let trials = trial_cuts(&played, from);
    let cuts = cuts(&played, from);
    assert_window(&played, from, &cuts);
    each(&cuts, |phase, after| {
        let events = resume(&played[phase], &model, after, Fate::PowerLoss);
        assert_unchanged_decisions(
            &played[phase],
            &model,
            &events,
            after,
            &events[after - 1].boot,
            None,
        );
        // Reset reason can change an open trial's classification. Outside its window both kinds take the
        // same recovery path, so the power-loss cut already covers it.
        if trials.contains(&(phase, after)) {
            resume(&played[phase], &model, after, Fate::Crashed);
        }
    });
    let smu: Vec<_> = played
        .iter()
        .enumerate()
        .skip(from)
        .flat_map(|(phase, played)| (1..=played.accesses.len()).map(move |at| (phase, at)))
        .collect();
    assert!(!smu.is_empty(), "the swept phases touched no SMU");
    each(&smu, |phase, at| {
        resume_at_smu(&played[phase], &model, at);
    });
    eprintln!(
        "crash coverage: {} power-loss durable events ({} dual-kind trial events), {} SMU hooks; no stride",
        cuts.len(),
        trials.len(),
        smu.len()
    );
}

/// The scenario's whole journal.
fn scenario_events() -> Vec<Event> {
    let (_, played, _) = scenario();
    played.last().unwrap().events.clone()
}

/// `events` with `change` applied to the first rate cell of the first `ccd.cap` that has one.
fn with_changed_rate_cell(events: &[Event], change: impl FnOnce(&mut RateCell)) -> Vec<Event> {
    let mut events = events.to_vec();
    let cell = events
        .iter_mut()
        .find_map(|event| {
            if let Payload::CcdCap(cap) = &mut event.payload {
                cap.cells.first_mut()
            } else {
                None
            }
        })
        .expect("no ccd.cap of the scenario carries a rate cell");
    change(cell);
    events
}

#[test_case(1, 0; "failure count")]
#[test_case(0, 1; "eligible seconds")]
fn a_changed_rate_cell_changes_the_commitments(failures: u32, eligible_s: u64) {
    let events = scenario_events();
    let changed = with_changed_rate_cell(&events, |cell| {
        cell.failures += failures;
        cell.eligible_s += eligible_s;
    });
    assert_ne!(
        commitments(&changed, 0, true),
        commitments(&events, 0, true),
        "a rate cell that counts {failures} more failures and {eligible_s} more seconds compares equal"
    );
}

#[test]
fn a_changed_count_of_cited_events_changes_the_projection() {
    let events = scenario_events();
    let projection = folded(&events).project();
    let mut changed = projection.clone();
    let (seq, step) = changed
        .day
        .last_step
        .as_mut()
        .expect("the scenario ends after a day step");
    step.hours.push(*seq);
    assert_ne!(
        compared_projection(&events, &changed),
        compared_projection(&events, &projection)
    );
}

#[test]
fn caps_citing_one_event_instead_of_two_change_the_projection() {
    let events = scenario_events();
    let projection = folded(&events).project();
    let witnesses: Vec<_> = projection
        .units
        .iter()
        .filter_map(|unit| unit.cap.as_ref().map(|cap| cap.witness))
        .collect();
    assert!(
        witnesses.len() >= 2 && witnesses[0] != witnesses[1],
        "the scenario's CCDs have no two distinct caps: {witnesses:?}"
    );
    let mut changed = projection.clone();
    let first = witnesses[0];
    for cap in changed.units.iter_mut().filter_map(|unit| unit.cap.as_mut()) {
        cap.witness = first;
    }
    assert_ne!(
        compared_projection(&events, &changed),
        compared_projection(&events, &projection)
    );
}

#[test]
fn a_qualification_citing_a_different_exposure_changes_the_commitments() {
    let events = scenario_events();
    let qualification = events
        .iter()
        .find(|event| matches!(&event.payload, Payload::NightQualification(record)
            if matches!(&record.state, QualificationState::Complete { witnesses } if !witnesses.is_empty())))
        .expect("the scenario completes a qualification");
    let Payload::NightQualification(record) = &qualification.payload else {
        unreachable!();
    };
    let QualificationState::Complete { witnesses } = &record.state else {
        unreachable!();
    };
    let cited: Vec<_> = witnesses
        .iter()
        .map(|witness| match witness.evidence {
            togi_journal::CellEvidence::Fresh { exposure }
            | togi_journal::CellEvidence::Reused { exposure } => exposure,
        })
        .collect();
    let replacement = events
        .iter()
        .find(|event| {
            matches!(event.payload, Payload::ExposureObserved(_)) && !cited.contains(&event.seq)
        })
        .expect("the scenario has an exposure outside this qualification");
    let original = events.iter().find(|event| event.seq == cited[0]).unwrap();
    let compared = vec![original.clone(), replacement.clone(), qualification.clone()];
    let mut changed = compared.clone();
    let Payload::NightQualification(record) = &mut changed[2].payload else {
        unreachable!();
    };
    let QualificationState::Complete { witnesses } = &mut record.state else {
        unreachable!();
    };
    match &mut witnesses[0].evidence {
        togi_journal::CellEvidence::Fresh { exposure }
        | togi_journal::CellEvidence::Reused { exposure } => *exposure = replacement.seq,
    }
    assert_ne!(
        commitments(&changed, 0, false),
        commitments(&compared, 0, false),
        "qualification witness changed from {} to {} with both exposures unchanged",
        original.seq,
        replacement.seq
    );
}

#[test]
fn a_cap_citing_a_different_event_changes_the_projection() {
    let events = scenario_events();
    let cap_at = events
        .iter()
        .position(|event| matches!(event.payload, Payload::CcdCap(_)))
        .expect("the scenario commits a cap");
    let events = &events[..=cap_at];
    let projection = folded(events).project();
    let mut changed = projection.clone();
    let replacement = events
        .iter()
        .find(|event| matches!(event.payload, Payload::TrialIntent(_)))
        .expect("the scenario runs a trial");
    let cap = changed.units[0]
        .cap
        .as_mut()
        .expect("the scenario has a cap");
    let original = cap.witness;
    cap.witness = replacement.seq;
    assert_ne!(
        compared_projection(events, &changed),
        compared_projection(events, &projection),
        "cap witness changed from {original} to {} with both target events unchanged",
        replacement.seq
    );
}
