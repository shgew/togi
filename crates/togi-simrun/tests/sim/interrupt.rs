use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::NonZeroU32;
use std::rc::Rc;

use togi_config::Config;
use togi_journal::fs::{AppendPoint, AppendTrigger, Fault, FileKind, Op};
use togi_journal::{
    Combination, CoreTransition, DeepeningRound, Event, EventKind, Failure, HuntEnd, HuntGroup,
    HuntStart, MemFs, Outcome, Payload, ProfileChange, RoundEvent, Seq, TrialId, TrialIntent,
    TunerDecision,
};
use togi_machine::{BootId, CoreId, Regime, ResetKind, Signal};
use togi_session::Stop;
use togi_sim::definition::JointDef;
use togi_sim::faults::{SmuOp, SmuOperation};
use togi_sim::{History, MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Observe, Replay, SimError, SimOutcome, simulate};

use test_case::test_case;

use crate::common::{Bench, finish, hunt_machine, nz, phase_two_concludes};

const SEED: u64 = 1;
const MAX_BOOTS: u32 = 60;
const QUICK: &str = r#"{
    "candidate_solo_limits": {"0": -10, "1": -10, "2": -10, "3": -10},
    "evidence": {"miss": 0.5},
    "durations": {
        "search_trial_s": 1,
        "short_trial_s": 1,
        "checking_trial_s": 1,
        "checking_idle_s": 1,
        "checking_all_core_s": 4
    }
}"#;

fn quick() -> Config {
    Config::parse(QUICK).unwrap()
}

#[derive(Debug, PartialEq)]
struct Hunt {
    start: HuntStart,
    failure: Option<Failure>,
    clean_cycle_parked: bool,
}

#[derive(Debug, PartialEq)]
struct Round {
    round: DeepeningRound,
    clean_cycle_base: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Trial {
    intent: TrialIntent,
    outcome: Outcome,
    signal: Option<Signal>,
    core: Option<CoreId>,
    duration_s: u32,
}

#[derive(Debug, Default, PartialEq)]
struct Commitments {
    attribution: Vec<Failure>,
    decisions: Vec<TunerDecision>,
    phases: Vec<CoreTransition>,
    failure_points: Vec<Combination>,
    profiles: Vec<ProfileChange>,
    hunts: Vec<Hunt>,
    groups: Vec<HuntGroup>,
    ends: Vec<HuntEnd>,
    rounds: Vec<Round>,
    checks: Vec<Trial>,
    reruns: Vec<Trial>,
}

fn anonymous(intent: &TrialIntent) -> TrialIntent {
    TrialIntent {
        trial: TrialId::from(String::new()),
        kernel_cursor: None,
        kernel_error: None,
        retry: false,
        ..intent.clone()
    }
}

fn commitments(events: &[Event]) -> Commitments {
    let mut out = Commitments::default();
    let mut intents: HashMap<TrialId, TrialIntent> = HashMap::new();
    let mut failures: HashMap<Seq, Failure> = HashMap::new();
    for event in events {
        match &event.payload {
            Payload::Failure(p) => {
                let failure = Failure {
                    trial: None,
                    ..p.clone()
                };
                failures.insert(event.seq, failure.clone());
                out.attribution.push(failure);
            }
            Payload::TunerDecision(p) => out.decisions.push(p.clone()),
            Payload::CorePhase(p) => out.phases.push(p.clone()),
            Payload::Combination(p) => out.failure_points.push(p.clone()),
            Payload::ProfileChange(p) => out.profiles.push(p.clone()),
            Payload::HuntStart(p) => out.hunts.push(Hunt {
                start: HuntStart {
                    trial: None,
                    failure: Seq::FIRST,
                    parked_seq: None,
                    ..p.clone()
                },
                failure: failures.get(&p.failure).cloned(),
                clean_cycle_parked: p.parked_seq.is_some(),
            }),
            Payload::HuntGroup(p) => out.groups.push(p.clone()),
            Payload::HuntEnd(p) => out.ends.push(p.clone()),
            Payload::DeepeningRound(p) => out.rounds.push(Round {
                round: DeepeningRound {
                    base_seq: None,
                    ..p.clone()
                },
                clean_cycle_base: p.base_seq.is_some(),
            }),
            Payload::TrialIntent(p) => {
                intents.insert(p.trial.clone(), anonymous(p));
            }
            Payload::TrialEnd(p) if p.outcome != Outcome::Inconclusive => {
                let Some(intent) = intents.get(&p.trial) else {
                    continue;
                };
                let trial = Trial {
                    intent: intent.clone(),
                    outcome: p.outcome,
                    signal: p.signal,
                    core: p.core,
                    duration_s: p.duration_s,
                };
                if intent.round.is_some() {
                    out.checks.push(trial.clone());
                }
                if intent.rerun {
                    out.reruns.push(trial);
                }
            }
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
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntSkipped(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
        }
    }
    out
}

fn assert_rerun_retries(events: &[Event]) {
    let mut intents: HashMap<&TrialId, &Event> = HashMap::new();
    let mut pending: HashMap<Seq, &Event> = HashMap::new();
    let mut completed: HashSet<&TrialId> = HashSet::new();
    for event in events {
        if let Payload::TrialIntent(p) = &event.payload
            && p.rerun
        {
            intents.insert(&p.trial, event);
        } else if let Payload::TrialEnd(p) = &event.payload {
            let Some(&intent_event) = intents.get(&p.trial) else {
                continue;
            };
            let obligation = *intent_event
                .cause
                .first()
                .unwrap_or_else(|| panic!("rerun {} has no obligation cause", p.trial));
            if p.outcome == Outcome::Inconclusive {
                pending.insert(obligation, intent_event);
                continue;
            }
            assert!(
                completed.insert(&p.trial),
                "rerun {} completed twice",
                p.trial
            );
            let Payload::TrialIntent(intent) = &intent_event.payload else {
                unreachable!("only rerun intents are tracked");
            };
            if intent.retry {
                let previous = pending.get(&obligation).unwrap_or_else(|| {
                    panic!("rerun retry {} has no interrupted obligation", p.trial)
                });
                let Payload::TrialIntent(before) = &previous.payload else {
                    unreachable!("only rerun intents are tracked");
                };
                assert!(
                    previous.cause == intent_event.cause && anonymous(before) == anonymous(intent),
                    "rerun retry {} changed its interrupted obligation",
                    p.trial
                );
            }
            pending.remove(&obligation);
        }
    }
}

fn assert_field<T: std::fmt::Debug + PartialEq>(label: &str, field: &str, want: &[T], got: &[T]) {
    let at = want
        .iter()
        .zip(got)
        .position(|(want, got)| want != got)
        .unwrap_or_else(|| want.len().min(got.len()));
    assert!(
        want.len() == got.len() && at == want.len(),
        "{label}: {field} differ from the uninterrupted run: {} wanted, {} got, first difference at {at}\nwant {:?}\ngot  {:?}",
        want.len(),
        got.len(),
        want.get(at),
        got.get(at),
    );
}

fn assert_decisions(want: &Commitments, events: &[Event], label: &str) {
    let got = commitments(events);
    assert_field(label, "attribution", &want.attribution, &got.attribution);
    assert_field(label, "decisions", &want.decisions, &got.decisions);
    assert_field(label, "phases", &want.phases, &got.phases);
    assert_field(
        label,
        "failure points",
        &want.failure_points,
        &got.failure_points,
    );
    assert_field(label, "profiles", &want.profiles, &got.profiles);
    assert_field(label, "hunts", &want.hunts, &got.hunts);
    assert_field(label, "groups", &want.groups, &got.groups);
    assert_field(label, "ends", &want.ends, &got.ends);
    assert_field(label, "rounds", &want.rounds, &got.rounds);
    assert_field(label, "checks", &want.checks, &got.checks);
    assert_field(label, "reruns", &want.reruns, &got.reruns);
    assert_rerun_retries(events);
    let mut seen: HashSet<(NonZeroU32, NonZeroU32)> = HashSet::new();
    for event in events {
        if let Payload::HuntGroup(group) = &event.payload {
            assert!(
                seen.insert((group.hunt, group.group)),
                "{label}: duplicated hunt.group ({}, {})",
                group.hunt,
                group.group
            );
        }
    }
}

type Closer = fn(&Event, &mut bool) -> bool;

fn hunt_opens(event: &Event) -> bool {
    event.kind() == EventKind::HuntStart
}

fn hunt_closes(event: &Event, closing: &mut bool) -> bool {
    if event.kind() == EventKind::HuntEnd {
        *closing = true;
    }
    *closing && matches!(&event.payload, Payload::TrialIntent(intent) if !intent.rerun)
}

fn deepening_opens(event: &Event) -> bool {
    matches!(&event.payload, Payload::DeepeningRound(round) if round.event == RoundEvent::Start)
}

fn deepening_closes(event: &Event, _: &mut bool) -> bool {
    matches!(&event.payload, Payload::DeepeningRound(round) if round.event == RoundEvent::End)
}

fn joint_machine() -> MachineDefinition {
    MachineDefinition {
        joint: vec![JointDef {
            members: BTreeMap::from([(1, -10), (3, -10)]),
            regimes: vec![Regime::R6],
            rate: Some(10.0),
            ..JointDef::default()
        }],
        ..hunt_machine(4)
    }
}

fn deep_machine() -> MachineDefinition {
    let mut definition = hunt_machine(4);
    for core in &mut definition.core {
        core.alone = [-50; 5];
        core.together = [-50; 7];
    }
    definition
}

fn run_until(bench: &Bench, mut open: impl FnMut(&Event) -> bool + 'static, crash_first: bool) {
    let mut input = bench.input();
    input.config = quick();
    input.policy.cycles = None;
    input.max_boots = nz(MAX_BOOTS);
    let opened = Rc::new(Cell::new(false));
    let (sim, seen, mut crashed) = (bench.sim.clone(), Rc::clone(&opened), !crash_first);
    input.observer = Some(Box::new(move |event| {
        if !crashed && event.kind() == EventKind::SessionStart {
            crashed = true;
            sim.crash();
        }
        if open(event) {
            seen.set(true);
            Observe::Stop
        } else {
            Observe::Continue
        }
    }));
    assert_eq!(finish(input).stop, Some(Stop::Signal));
    assert!(opened.get(), "the prefix never opened its window");
}

#[derive(Clone, Copy)]
enum Interruption {
    Never,
    Append(usize),
    Smu(usize),
}

struct Run {
    events: Vec<Event>,
    accesses: Vec<SmuOperation>,
    interrupted: bool,
    closed: bool,
}

struct Case {
    name: &'static str,
    prefix: Bench,
    definition: MachineDefinition,
    prefix_len: usize,
    closer: Closer,
    reference: Run,
    want: Commitments,
    close_at: usize,
}

impl Case {
    fn new(
        name: &'static str,
        prefix: Bench,
        definition: MachineDefinition,
        closer: Closer,
    ) -> Self {
        assert!(
            prefix.events().iter().any(|event| matches!(
                &event.payload,
                Payload::CrashDetected(crash) if crash.reset_reason.is_some()
            )),
            "{name}: no earlier boot logged a reset reason"
        );
        let prefix_len = prefix.events().len();
        let mut case = Self {
            name,
            prefix,
            definition,
            prefix_len,
            closer,
            reference: Run {
                events: Vec::new(),
                accesses: Vec::new(),
                interrupted: false,
                closed: false,
            },
            want: Commitments::default(),
            close_at: 0,
        };
        case.reference = case.run(Interruption::Never);
        assert!(
            case.reference.closed,
            "{name}: the reference never closed its window"
        );
        case.want = commitments(&case.reference.events);
        assert!(
            !case.reference.accesses.is_empty(),
            "{name}: the reference touched no SMU"
        );
        let mut closing = false;
        case.close_at = case.reference.events[prefix_len..]
            .iter()
            .position(|event| closer(event, &mut closing))
            .map(|index| index + 1)
            .unwrap_or_else(|| panic!("{name}: the reference has no closing event"));
        case
    }

    /// Runs a boot that loses power the moment its `at`th append lands, before any cleanup can touch the SMU, and
    /// asserts that append is the boot's final event and that nothing wrote the SMU afterwards. Power is lost
    /// before the first append when `at` is 0. A write fault only keeps the already crashed boot from journaling more.
    fn lose_power_at_append(&self, bench: &Bench, at: usize) {
        let label = format!("{} {}", self.name, Interruption::Append(at).label());
        let trigger = AppendTrigger::new(self.prefix_len + at, AppendPoint::Landed);
        let writes_after = Rc::new(Cell::new(0_usize));
        let (landed, writes) = (trigger.clone(), Rc::clone(&writes_after));
        bench.sim.interrupt_smu(move |operation: &SmuOperation| {
            if landed.fired() && operation.op != SmuOp::Read {
                writes.set(writes.get() + 1);
            }
            Ok(())
        });
        let mut input = bench.input();
        input.config = quick();
        input.policy.cycles = None;
        input.replay = Replay::Cold;
        input.max_boots = nz(1);
        if at == 0 {
            bench.sim.power_loss();
        } else {
            assert!(trigger.arm(&bench.fs, self.prefix_len));
            let (sim, fs, trigger) = (bench.sim.clone(), bench.fs.clone(), trigger.clone());
            input.observer = Some(Box::new(move |event| {
                if trigger.lands(event) {
                    sim.power_loss();
                    fs.inject(Fault::before(Op::Write, 1).named("events.jsonl"));
                }
                Observe::Continue
            }));
        }
        let rebooted = match simulate(input) {
            Ok(SimOutcome::Capped { .. }) => true,
            Ok(SimOutcome::Stopped(finished)) if !finished.errors.is_empty() => false,
            Err(SimError::Failed { .. } | SimError::Host { .. }) => false,
            other => panic!(
                "{label}: the boot did not end on the crash: {:?}",
                other.map(drop)
            ),
        };
        bench.fs.clear_faults();
        assert_eq!(
            trigger.disarm(&bench.fs),
            at > 0,
            "{label}: the selected append never landed"
        );
        assert_eq!(
            bench.events().len(),
            self.prefix_len + at,
            "{label}: the boot did not end at the selected append"
        );
        assert_eq!(
            writes_after.get(),
            0,
            "{label}: cleanup wrote the SMU after power was lost"
        );
        if !rebooted {
            bench.sim.reboot().unwrap();
        }
    }

    fn fork(&self) -> Bench {
        let fs = MemFs::new();
        for (path, kind) in self.prefix.fs.entries() {
            if kind == FileKind::File {
                fs.put(&path, self.prefix.fs.contents(&path).unwrap());
            }
        }
        let probe = Bench {
            sim: self.prefix.sim.clone(),
            fs,
        };
        let mut boots: Vec<BootId> = Vec::new();
        let mut resets: HashMap<BootId, ResetKind> = HashMap::new();
        for event in probe.events() {
            if !boots.contains(&event.boot) {
                boots.push(event.boot.clone());
            }
            if let Payload::CrashDetected(crash) = &event.payload {
                resets.insert(
                    event.boot.clone(),
                    crash.reset_reason.unwrap_or(ResetKind::PowerLoss),
                );
            }
        }
        let history = togi_simrun::history_in(&probe.dir()).unwrap();
        let sim = Simulator::new(
            Model::new(&self.definition, SEED).unwrap(),
            RunSettings {
                seed: SEED,
                history: History {
                    boots: 0,
                    ..history
                },
                replay: None,
            },
        )
        .unwrap();
        for next in boots.iter().skip(1).map(Some).chain([None]) {
            if let Some(&kind) = next.and_then(|boot| resets.get(boot)) {
                sim.next_reset(kind);
            }
            sim.reboot().unwrap();
        }
        Bench { sim, ..probe }
    }

    fn run(&self, interruption: Interruption) -> Run {
        let bench = self.fork();
        let accesses: Rc<RefCell<Vec<SmuOperation>>> = Rc::default();
        let interrupted = Rc::new(Cell::new(false));
        let closed = Rc::new(Cell::new(false));
        let mut closing = false;
        if let Interruption::Append(at) = interruption {
            interrupted.set(true);
            self.lose_power_at_append(&bench, at);
            for event in &bench.events()[self.prefix_len..] {
                if !closed.get() && (self.closer)(event, &mut closing) {
                    closed.set(true);
                }
            }
        }
        let mut input = bench.input();
        input.config = quick();
        input.policy.cycles = None;
        input.replay = Replay::Cold;
        input.max_boots = nz(MAX_BOOTS);
        let cancel = input.cancel.clone();

        let sim = bench.sim.clone();
        let (seen, fired) = (Rc::clone(&accesses), Rc::clone(&interrupted));
        bench.sim.interrupt_smu(move |operation: &SmuOperation| {
            let mut seen = seen.borrow_mut();
            seen.push(*operation);
            if matches!(interruption, Interruption::Smu(at) if at == seen.len()) && !fired.get() {
                fired.set(true);
                sim.power_loss();
                return Err(togi_machine::Error::Crashed);
            }
            Ok(())
        });
        let (closer, done) = (self.closer, Rc::clone(&closed));
        input.observer = Some(Box::new(move |event| {
            if !done.get() && closer(event, &mut closing) {
                done.set(true);
            }
            if done.get() {
                cancel.cancel();
            }
            Observe::Continue
        }));

        let finished = finish(input);
        let label = format!("{} {}", self.name, interruption.label());
        assert_eq!(
            finished.stop,
            Some(Stop::Signal),
            "{label}: {:?}",
            finished.errors
        );
        assert!(
            bench.sim.violations().is_empty(),
            "{label}: {:?}",
            bench.sim.violations()
        );
        Run {
            events: bench.events(),
            accesses: accesses.take(),
            interrupted: interrupted.get(),
            closed: closed.get(),
        }
    }

    fn assert_interruption(&self, interruption: Interruption) {
        let label = format!("{} {}", self.name, interruption.label());
        let run = self.run(interruption);
        assert!(run.closed, "{label}: the window never closed");
        assert!(run.interrupted, "{label}: did not interrupt");
        if let Interruption::Smu(at) = interruption {
            assert_eq!(
                run.accesses[at - 1],
                self.reference.accesses[at - 1],
                "{label}: interrupted another SMU access"
            );
        }
        if !matches!(interruption, Interruption::Append(0)) {
            assert!(
                run.events[self.prefix_len..]
                    .iter()
                    .any(|event| event.kind() == EventKind::CrashDetected),
                "{label}: the resumed boot detected no crash"
            );
        }
        assert_decisions(&self.want, &run.events, &label);
    }

    fn append_points(&self) -> usize {
        self.close_at + 1
    }

    fn smu_points(&self) -> usize {
        self.reference.accesses.len()
    }
}

impl Interruption {
    fn label(self) -> String {
        match self {
            Self::Never => "uninterrupted".into(),
            Self::Append(at) => format!("append {at}"),
            Self::Smu(at) => format!("smu {at}"),
        }
    }
}

fn hunt_case() -> Case {
    let quiet = Bench::new(&hunt_machine(4), SEED);
    run_until(&quiet, phase_two_concludes(&quiet), false);
    let prefix = quiet.resumed(&joint_machine(), SEED);
    run_until(&prefix, hunt_opens, false);
    let case = Case::new("hunt", prefix, joint_machine(), hunt_closes);
    assert!(
        !case.want.hunts.is_empty(),
        "the hunt reference has no hunt"
    );
    assert!(
        !case.want.groups.is_empty(),
        "the hunt reference has no group"
    );
    assert!(!case.want.ends.is_empty(), "the hunt reference has no end");
    assert!(
        !case.want.reruns.is_empty(),
        "the hunt reference has no completed rerun evidence"
    );
    for rerun in &case.want.reruns {
        assert_eq!(rerun.outcome, Outcome::Pass, "{rerun:?}");
    }
    case
}

fn deepening_case() -> Case {
    let prefix = Bench::new(&deep_machine(), SEED);
    run_until(&prefix, deepening_opens, true);
    let case = Case::new("deepening", prefix, deep_machine(), deepening_closes);
    assert!(
        case.want
            .rounds
            .iter()
            .any(|round| round.round.event == RoundEvent::End),
        "the deepening reference closed no round"
    );
    case
}

const SHARES: usize = 4;

#[test_case(hunt_case, 0; "hunt resume 0")]
#[test_case(hunt_case, 1; "hunt resume 1")]
#[test_case(hunt_case, 2; "hunt resume 2")]
#[test_case(hunt_case, 3; "hunt resume 3")]
#[test_case(deepening_case, 0; "deepening resume 0")]
#[test_case(deepening_case, 1; "deepening resume 1")]
#[test_case(deepening_case, 2; "deepening resume 2")]
#[test_case(deepening_case, 3; "deepening resume 3")]
fn power_loss_during(case: fn() -> Case, index: usize) {
    let case = case();
    for at in (index..case.append_points()).step_by(SHARES) {
        case.assert_interruption(Interruption::Append(at));
    }
    for at in (index + 1..=case.smu_points()).step_by(SHARES) {
        case.assert_interruption(Interruption::Smu(at));
    }
}
