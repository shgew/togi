//! Cutpoint sweeps over Ruleset 13: the journal of a scenario of nights and days is cut after each of its durable
//! events in turn, and the scenario resumed from the cut must reach what the uninterrupted scenario reached.
//!
//! Nights run through the public `run`. Days run through [`day`], a day host that, like `togi day`, derives
//! everything it writes from the journal and the policy state it folds, so a day cut after any record resumes where
//! it stopped.
//!
//! The swept window starts with the run that completes night 2's qualification and ends with the final day:
//! all 380 durable events, without a stride. Projection comparison blanks only shifted sequence, trial and night
//! references (including `last_step`'s sequence); policy commitments are compared as a sorted multiset.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::Value;
use togi_config::Config;
use togi_journal::fs::{AppendPoint, AppendTrigger, FileKind, Fs, MemFs};
use togi_journal::{
    ApplicationId, ApplyCause, CcdVector, DayApplied, DayBootStart, DayCheckpoint, DayCleanStop,
    DayOffsetsChanged, DayUncleanEnd, Event, Journal, Payload, QualificationState, Seq, Stamp,
    TrialId,
};
use togi_machine::{
    BootId, CancellationToken, EndEvidence, Machine, ObservedOffset, PerCore, ResetReason,
};
use togi_sim::definition::CoreDef;
use togi_sim::{MachineDefinition, Model, Persisted, Simulator};
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::policy::{DayQuery, Params, State};

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

fn read_offsets(machine: &mut Machine<'_>, state: &State) -> PerCore<ObservedOffset> {
    state
        .project()
        .ccds
        .iter()
        .flat_map(|ccd| ccd.cores.iter())
        .map(|core| (core, machine.smu.offset(core).unwrap()))
        .collect()
}

/// The day host's journal and the policy state folded from it.
struct DayJournal {
    journal: Journal,
    state: State,
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
fn day(bench: &Bench, trigger: Option<&AppendTrigger>, hours: u64, clean: bool) -> bool {
    let mut machine = bench.sim.seams();
    let boot = machine.host.boot_id().unwrap();
    let journal = simulated(&bench.mem)
        .open_at(boot.clone(), bench.sim.now())
        .unwrap();
    if let Some(trigger) = trigger {
        trigger.arm(&bench.mem, journal.events().len());
    }
    let state = folded(journal.events());
    let mut day = DayJournal { journal, state };
    let ran = (|| {
        if !day
            .in_boot(&boot)
            .any(|event| matches!(event.payload, Payload::DayBootStart(_)))
        {
            let offsets = read_offsets(&mut machine, &day.state);
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
                let applied = day.state.project().applied.unwrap();
                let cores = machine.host.topology().unwrap();
                let target = CcdVector {
                    cores: applied.expand(&cores).unwrap(),
                    ccd: applied,
                };
                (ApplyCause::Reapplication {}, target)
            };
            for (core, offset) in target.cores.iter() {
                machine.smu.set_offset(core, offset).unwrap();
            }
            let readbacks = read_offsets(&mut machine, &day.state);
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
        let readbacks = read_offsets(&mut machine, &day.state);
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
    assert_eq!(ran, !fired, "a day append failed without the cut");
    if ran {
        drop(day.journal.close().unwrap());
    }
    ran
}

/// A run of the session on the current boot that the operator ends once `trials` trials ended uninterrupted in the
/// journal after its first `base` events.
fn segment(bench: &Bench, trigger: Option<&AppendTrigger>, base: usize, trials: usize) -> RunEnd {
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
    input.on_append = Some(Box::new(move |event| {
        if ended_whole(event) {
            whole += 1;
            if whole >= trials {
                cancel.cancel();
            }
        }
    }));
    if let Some(trigger) = trigger {
        trigger.arm(&bench.mem, input.journal.events().len());
    }
    let ended = crate::run(input);
    if trigger.is_some_and(|trigger| trigger.disarm(&bench.mem)) {
        assert!(
            ended.capsule.is_none(),
            "the cut event failed, yet the run went on"
        );
        return RunEnd::Cut(ended.errors.iter().any(crate::SessionError::is_crash));
    }
    if ended.errors.iter().any(crate::SessionError::is_crash) {
        return RunEnd::Crashed;
    }
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let stop = ended.stop;
    drop(ended.journal.close().unwrap());
    RunEnd::Stopped(stop)
}

enum RunEnd {
    Stopped(Option<Stop>),
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
/// is given and `fate` befalling the machine there. Returns how the last run stopped, if the phase ran one.
fn play(bench: &Bench, phase: Phase, base: usize, cut: Option<(usize, Fate)>) -> Option<Stop> {
    let trigger = cut.map(|(after, _)| AppendTrigger::new(after, AppendPoint::BeforeSync));
    let fate = cut.map_or(Fate::Killed, |(_, fate)| fate);
    for _ in 0..BOOTS {
        let finished = match phase {
            Phase::Boot => {
                bench.sim.reboot().unwrap();
                return None;
            }
            Phase::Run { trials } => match segment(bench, trigger.as_ref(), base, trials) {
                RunEnd::Stopped(stop) => return stop,
                RunEnd::Crashed => {
                    bench.sim.reboot().unwrap();
                    false
                }
                RunEnd::Cut(crashed) => {
                    assert_eq!(durable_lines(bench), cut.unwrap().0);
                    if crashed || matches!(fate, Fate::Crashed) {
                        bench.sim.crash();
                        bench.sim.reboot().unwrap();
                    }
                    false
                }
            },
            Phase::Day { hours } => day(bench, trigger.as_ref(), hours, true),
            Phase::Crash { hours } => {
                let ran = day(bench, trigger.as_ref(), hours, false);
                if ran {
                    bench.sim.crash();
                    bench.sim.reboot().unwrap();
                }
                ran
            }
        };
        if finished {
            return None;
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
    let stop = play(bench, phase, start, None);
    played.push(Played {
        phase,
        before,
        start,
        events: durable(bench),
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

/// Runs `check` on every cut in parallel, and fails with every cut it failed on.
fn each(cuts: &[(usize, usize)], check: impl Fn(usize, usize) + Sync) {
    let next = AtomicUsize::new(0);
    let failed = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(4, NonZeroUsize::get);
    std::thread::scope(|scope| {
        for _ in 0..workers {
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

/// Plays `played`'s phase from its start with the journal cut after its `after`th event and `fate` befalling the
/// machine there, and checks that the journal left is resumable. Returns the journal at the phase's end.
fn resume(played: &Played, model: &Model, after: usize, fate: Fate) -> Vec<Event> {
    let bench = played.before.bench(model);
    play(&bench, played.phase, played.start, Some((after, fate)));
    let events = durable(&bench);
    let facts = togi_journal::State::replay(&events).unwrap();
    assert!(facts.open.is_empty(), "left open: {:?}", facts.open);
    folded(&events);
    assert!(
        bench.sim.violations().is_empty(),
        "{:?}",
        bench.sim.violations()
    );
    events
}

/// Fields that hold sequence numbers, trial IDs or night IDs, which a resumed journal shifts by the events the cut
/// added.
const SHIFTED: &[&str] = &[
    "caps",
    "checkpoints",
    "closed_by",
    "credited",
    "events",
    "evidence",
    "exposure",
    "failure",
    "failures",
    "hours",
    "last_checkpoint",
    "night",
    "qualification",
    "retreat",
    "seq",
    "since",
    "transactions",
    "trial",
    "vetoes",
    "witness",
    "witnesses",
];

/// `value` with every [`SHIFTED`] reference blanked.
fn unshifted(mut value: Value) -> Value {
    fn blank(value: &mut Value) {
        match value {
            Value::Object(fields) => {
                for (key, field) in fields.iter_mut() {
                    let reference = |value: &Value| value.is_number() || value.is_string();
                    let shifted = SHIFTED.contains(&key.as_str())
                        && (reference(field)
                            || field
                                .as_array()
                                .is_some_and(|items| items.iter().all(reference)));
                    if shifted {
                        *field = Value::Null;
                    } else if key == "last_step"
                        && let Some(step) = field.as_array_mut()
                    {
                        step[0] = Value::Null;
                        step.iter_mut().for_each(blank);
                    } else {
                        blank(field);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(blank),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    blank(&mut value);
    value
}

/// The tuner's projection of `events`, references blanked.
fn projection(events: &[Event]) -> Value {
    unshifted(serde_json::to_value(folded(events).project()).unwrap())
}

/// The policy commitments among `events` in a canonical order, references blanked, without the observations of
/// trials a cut interrupted, which only the resumed journal has. Such an observation may also commit an unchanged
/// cap before the retried trial rather than after it, so the order of the commitments is not compared.
fn commitments(events: &[Event]) -> Vec<String> {
    let interrupted: Vec<_> = events
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
    let mut commitments: Vec<_> = events
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
            ) || matches!(&event.payload, Payload::ExposureObserved(observed)
                if observed.source.trial.as_ref().is_none_or(|trial| !interrupted.contains(&trial)))
        })
        .map(|event| {
            let payload = serde_json::to_value(&event.payload).unwrap();
            format!("{} {}", event.kind().as_str(), unshifted(payload))
        })
        .collect();
    commitments.sort_unstable();
    commitments
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

#[test]
fn killing_the_scenario_after_every_durable_event_resumes_to_the_same_projection_and_commitments() {
    let (model, played, from) = scenario();
    let cuts = cuts(&played, from);
    assert_eq!(cuts.len(), 380, "the durable-event sweep window changed");
    let cut: BTreeSet<_> = cuts
        .iter()
        .map(|&(phase, after)| played[phase].events[after - 1].kind().as_str())
        .collect();
    let missed: Vec<_> = KINDS.iter().filter(|kind| !cut.contains(*kind)).collect();
    assert!(missed.is_empty(), "no cut after {missed:?}");
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
            commitments(&events[after..]),
            commitments(&reference.events[after..])
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
fn a_crash_at_every_trial_event_leaves_a_resumable_journal() {
    let (model, played, from) = scenario();
    let cuts = trial_cuts(&played, from);
    assert!(!cuts.is_empty(), "the swept phases ran no trial");
    each(&cuts, |phase, after| {
        resume(&played[phase], &model, after, Fate::Crashed);
    });
}
