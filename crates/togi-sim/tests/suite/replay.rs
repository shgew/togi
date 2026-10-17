// Replay facts: validation, matching and the uniform choice among matches.

use std::collections::BTreeSet;
use std::time::Duration;
use togi_machine::{BiosContext, CoreSet, Regime, Signal, TargetOffset, workload_by_id};
use togi_sim::random::{Domain, stream};
use togi_sim::replay::{FactProblem, InvalidFact, Replay, ReplayFact, ReplayOutcome};
use togi_sim::{Load, RunSettings};

use test_case::test_case;

use crate::common::{
    Rig, cores, crashed, flat_cores, machine, model, offsets, r7_workload, sharp, spec,
};

fn context(version: &str) -> BiosContext {
    BiosContext {
        bios_version: version.to_owned(),
        board: "board".to_owned(),
        cpu_model: "cpu".to_owned(),
        microcode: "0x1".to_owned(),
        boost_limit_mhz: 5700,
    }
}

fn fact(outcome: ReplayOutcome) -> ReplayFact {
    ReplayFact {
        context: context("A"),
        regime: Regime::R7,
        workload: "work".to_owned(),
        cores: cores(&[1, 0]),
        duration: Duration::from_secs(60),
        profile: offsets(&[-20, -21]),
        outcome,
    }
}

fn failure(signal: Signal, seconds: u64) -> ReplayOutcome {
    ReplayOutcome::Failure {
        signal,
        exposure: Duration::from_secs(seconds),
    }
}

fn trial(regime: Regime, workload: &str, cores: CoreSet, seconds: u64) -> Load<'_> {
    Load {
        regime,
        workload,
        cores,
        duration: Duration::from_secs(seconds),
    }
}

fn pick(
    replay: &Replay,
    load: &Load<'_>,
    profile: &[TargetOffset],
    seed: u64,
) -> Option<ReplayOutcome> {
    let mut rng = stream(seed, Domain::Replay, &[4, 7]);
    replay
        .answer(&context("A"), load, profile, &mut rng)
        .map(|fact| fact.outcome.clone())
}

fn answers() -> Replay {
    let mut other_profile = fact(failure(Signal::ComputationError, 13));
    other_profile.profile = offsets(&[-21, -21]);
    let mut other_context = fact(failure(Signal::ComputationError, 11));
    other_context.context = context("B");
    Replay::new([
        fact(ReplayOutcome::Pass),
        fact(failure(Signal::Stall, 7)),
        fact(failure(Signal::UnexpectedExit, 9)),
        other_context,
        other_profile,
    ])
    .expect("valid facts")
}

#[test]
fn a_match_is_drawn_among_the_facts_of_its_context_and_full_profile() {
    let replay = answers();
    assert_eq!(replay.len(), 5);
    let regime = Regime::R7;
    let load = trial(regime, "work", cores(&[0, 1]), 60);
    let profile = offsets(&[-20, -21]);
    let mut seen = BTreeSet::new();
    for seed in 1..=64 {
        let first = pick(&replay, &load, &profile, seed).expect("an answer");
        assert_eq!(
            pick(&replay, &load, &profile, seed),
            Some(first.clone()),
            "deterministic at seed {seed}"
        );
        seen.insert(format!("{first:?}"));
    }
    let want: BTreeSet<_> = [
        ReplayOutcome::Pass,
        failure(Signal::Stall, 7),
        failure(Signal::UnexpectedExit, 9),
    ]
    .iter()
    .map(|outcome| format!("{outcome:?}"))
    .collect();
    assert_eq!(seen, want, "other contexts and profiles never answer");
}

#[test]
fn the_choice_among_matches_is_uniform() {
    let replay = answers();
    let regime = Regime::R7;
    let load = trial(regime, "work", cores(&[0, 1]), 60);
    let profile = offsets(&[-20, -21]);
    let mut counts = [0u32; 3];
    for seed in 0..3000 {
        let outcome = pick(&replay, &load, &profile, seed).expect("an answer");
        let slot = match outcome {
            ReplayOutcome::Pass => 0,
            ReplayOutcome::Failure {
                signal: Signal::Stall,
                ..
            } => 1,
            ReplayOutcome::Failure { .. } => 2,
        };
        counts[slot] += 1;
    }
    for count in counts {
        assert!((850..=1150).contains(&count), "{counts:?}");
    }
}

#[test]
fn the_choice_does_not_depend_on_the_index_hash_order() {
    let facts: Vec<_> = (1..=9)
        .map(|seconds| fact(failure(Signal::Stall, seconds)))
        .collect();
    let first = Replay::new(facts.clone()).expect("valid");
    let second = Replay::new(facts).expect("valid");
    let regime = Regime::R7;
    let load = trial(regime, "work", cores(&[0, 1]), 60);
    let profile = offsets(&[-20, -21]);
    for seed in 0..50 {
        assert_eq!(
            pick(&first, &load, &profile, seed),
            pick(&second, &load, &profile, seed),
            "seed {seed}"
        );
    }
}

#[test]
fn the_recorded_key_has_an_answer() {
    let replay = Replay::new([fact(ReplayOutcome::Pass)]).expect("valid");
    let base = trial(Regime::R7, "work", cores(&[0, 1]), 60);
    assert!(replay.has_answer(&context("A"), &base, &offsets(&[-20, -21])));
}

#[test_case(trial(Regime::R2, "work", cores(&[0, 1]), 60), &offsets(&[-20, -21]), &context("A"); "regime")]
#[test_case(trial(Regime::R7, "other", cores(&[0, 1]), 60), &offsets(&[-20, -21]), &context("A"); "workload")]
#[test_case(trial(Regime::R7, "work", cores(&[1]), 60), &offsets(&[-20, -21]), &context("A"); "cores")]
#[test_case(trial(Regime::R7, "work", cores(&[0, 1, 2]), 60), &offsets(&[-20, -21, 0]), &context("A"); "more cores")]
#[test_case(trial(Regime::R7, "work", CoreSet::default(), 60), &offsets(&[-20, -21]), &context("A"); "idle")]
#[test_case(trial(Regime::R7, "work", cores(&[0, 1]), 90), &offsets(&[-20, -21]), &context("A"); "duration")]
#[test_case(trial(Regime::R7, "work", cores(&[0, 1]), 60), &offsets(&[-21, -21]), &context("A"); "loaded offset")]
#[test_case(trial(Regime::R7, "work", cores(&[0, 1]), 60), &offsets(&[-20, -22]), &context("A"); "other loaded offset")]
#[test_case(trial(Regime::R7, "work", cores(&[0, 1]), 60), &offsets(&[-20, -21, 0]), &context("A"); "profile length")]
#[test_case(trial(Regime::R7, "work", cores(&[0, 1]), 60), &offsets(&[-20, -21]), &context("B"); "context")]
fn any_difference_in_the_key_leaves_the_trial_to_the_fitted_model(
    load: Load<'_>,
    profile: &[TargetOffset],
    context: &BiosContext,
) {
    let replay = Replay::new([fact(ReplayOutcome::Pass)]).expect("valid");
    assert!(!replay.has_answer(context, &load, profile));
    let mut rng = stream(1, Domain::Replay, &[]);
    assert!(replay.answer(context, &load, profile, &mut rng).is_none());
}

#[test]
fn an_offset_of_an_unloaded_core_is_part_of_the_key() {
    let mut recorded = fact(ReplayOutcome::Pass);
    recorded.profile = offsets(&[-20, -21, -5]);
    let replay = Replay::new([recorded]).expect("valid");
    let load = trial(Regime::R7, "work", cores(&[0, 1]), 60);
    let profile = offsets(&[-20, -21, -5]);
    assert!(replay.has_answer(&context("A"), &load, &profile));
    let mut rng = stream(1, Domain::Replay, &[]);
    assert!(
        replay
            .answer(&context("A"), &load, &profile, &mut rng)
            .is_some()
    );
    // Only core 2 differs, and core 2 is not loaded.
    let changed = offsets(&[-20, -21, -6]);
    assert!(!replay.has_answer(&context("A"), &load, &changed));
    assert!(
        replay
            .answer(&context("A"), &load, &changed, &mut rng)
            .is_none()
    );
}

/// One trial of a machine whose fitted model fails it at once, with `replay` if given: its debug-printed
/// outcome, the virtual time it took and whether the machine crashed.
fn run_against(replay: Option<Replay>, signal: Signal, key: &MissKey) -> (String, Duration, bool) {
    let mut definition = machine(2, flat_cores(2, -10, -10));
    definition.model = sharp(signal);
    definition.bios = Some(vec![-20, -21]);
    let model = model(&definition);
    let mut rig = Rig::with(
        model,
        RunSettings {
            seed: 5,
            replay,
            ..RunSettings::default()
        },
    );
    if key.change_profile {
        rig.set(1, -22).expect("set");
    }
    if key.change_context {
        rig.sim.set_bios_context(context("B"));
    }
    let work = workload_by_id(key.workload).expect("a catalog workload");
    let (waited, _) = rig.run(spec(
        "1",
        key.regime,
        work,
        key.cores,
        Duration::from_secs(key.seconds),
    ));
    let crash = crashed(&waited);
    (format!("{waited:?}"), rig.sim.monotonic(), crash)
}

#[derive(Clone, Copy)]
struct MissKey {
    regime: Regime,
    workload: &'static str,
    cores: &'static [i64],
    seconds: u64,
    change_profile: bool,
    change_context: bool,
}

fn hit() -> MissKey {
    MissKey {
        regime: Regime::R7,
        workload: r7_workload(0),
        cores: &[0, 1],
        seconds: 60,
        change_profile: false,
        change_context: false,
    }
}

/// A recorded pass for the key `hit` names on the machine `run_against` builds.
fn recorded_pass() -> Replay {
    let mut definition = machine(2, flat_cores(2, -10, -10));
    definition.bios = Some(vec![-20, -21]);
    let mut recorded = fact(ReplayOutcome::Pass);
    r7_workload(0).clone_into(&mut recorded.workload);
    recorded.context = model(&definition).bios_context().clone();
    Replay::new([recorded]).expect("valid")
}

#[test_case(Signal::ComputationError; "computation error")]
#[test_case(Signal::Crash; "crash")]
fn a_replay_hit_answers_the_recorded_pass_and_the_fitted_failure_never_runs(signal: Signal) {
    let hit_run = run_against(Some(recorded_pass()), signal, &hit());
    let model_only = run_against(None, signal, &hit());
    assert_ne!(hit_run, model_only, "the recorded pass is used");
    assert_eq!(
        model_only.2,
        signal == Signal::Crash,
        "the model fails the trial"
    );
    assert!(model_only.1 < Duration::from_secs(60), "and does so early");
}

#[test_case(Signal::ComputationError, MissKey { regime: Regime::R2, ..hit() }; "computation error regime")]
#[test_case(Signal::ComputationError, MissKey { workload: r7_workload(1), ..hit() }; "computation error workload")]
#[test_case(Signal::ComputationError, MissKey { cores: &[1], ..hit() }; "computation error cores")]
#[test_case(Signal::ComputationError, MissKey { seconds: 90, ..hit() }; "computation error duration")]
#[test_case(Signal::ComputationError, MissKey { change_profile: true, ..hit() }; "computation error profile")]
#[test_case(Signal::ComputationError, MissKey { change_context: true, ..hit() }; "computation error context")]
#[test_case(Signal::Crash, MissKey { regime: Regime::R2, ..hit() }; "crash regime")]
#[test_case(Signal::Crash, MissKey { workload: r7_workload(1), ..hit() }; "crash workload")]
#[test_case(Signal::Crash, MissKey { cores: &[1], ..hit() }; "crash cores")]
#[test_case(Signal::Crash, MissKey { seconds: 90, ..hit() }; "crash duration")]
#[test_case(Signal::Crash, MissKey { change_profile: true, ..hit() }; "crash profile")]
#[test_case(Signal::Crash, MissKey { change_context: true, ..hit() }; "crash context")]
fn a_replay_miss_runs_the_fitted_failure_model_exactly_as_if_there_were_no_replay(
    signal: Signal,
    key: MissKey,
) {
    let missed = run_against(Some(recorded_pass()), signal, &key);
    let model_only = run_against(None, signal, &key);
    assert_eq!(missed, model_only);
    assert!(
        missed.1 < Duration::from_secs(key.seconds),
        "the trial did not fail"
    );
}

#[test]
fn condition_and_phase_are_not_part_of_the_key() {
    let replay = Replay::new([fact(ReplayOutcome::Pass)]).expect("valid");
    let regime = Regime::R7;
    let load = trial(regime, "work", cores(&[0, 1]), 60);
    assert!(replay.has_answer(&context("A"), &load, &offsets(&[-20, -21])));
}

type Edit = fn(&mut ReplayFact);

fn rejected(edit: Edit) -> InvalidFact {
    let mut bad = fact(failure(Signal::Stall, 7));
    edit(&mut bad);
    Replay::new([fact(ReplayOutcome::Pass), bad]).expect_err("an invalid fact")
}

#[test_case(|f| f.context = BiosContext::default(), FactProblem::NoContext; "no context")]
#[test_case(|f| f.cores = CoreSet::default(), FactProblem::NoCores; "no cores")]
#[test_case(
    |f| f.cores = cores(&[0, 2]),
    FactProblem::CoreOutsideProfile { core: 2, cores: 2 };
    "a core outside the profile"
)]
#[test_case(|f| f.duration = Duration::ZERO, FactProblem::NoDuration; "no duration")]
#[test_case(|f| f.outcome = failure(Signal::Stall, 61), FactProblem::ExposureTooLong; "an exposure beyond the duration")]
fn facts_with_invalid_evidence_are_refused_with_their_index(edit: Edit, problem: FactProblem) {
    let error = rejected(edit);
    assert_eq!((error.index, error.problem), (1, problem));
}

#[test_case(Signal::ComputationError; "computation error")]
#[test_case(Signal::Stall; "stall")]
#[test_case(Signal::UnexpectedExit; "unexpected exit")]
#[test_case(Signal::CorrectedMce; "corrected mce")]
#[test_case(Signal::UncorrectedMce; "uncorrected mce")]
#[test_case(Signal::Crash; "crash")]
fn every_failure_signal_the_extract_can_hold_is_accepted_with_a_zero_exposure(signal: Signal) {
    assert!(Replay::new([fact(failure(signal, 0))]).is_ok());
}

#[test]
fn an_exposure_equal_to_the_duration_is_valid() {
    assert!(Replay::new([fact(failure(Signal::Crash, 60))]).is_ok());
}

#[test]
fn facts_of_several_contexts_share_one_index() {
    let mut other = fact(failure(Signal::Crash, 5));
    other.context = context("B");
    let replay = Replay::new([fact(ReplayOutcome::Pass), other]).expect("valid");
    let regime = Regime::R7;
    let load = trial(regime, "work", cores(&[0, 1]), 60);
    let profile = offsets(&[-20, -21]);
    assert!(replay.has_answer(&context("A"), &load, &profile));
    assert!(replay.has_answer(&context("B"), &load, &profile));
    assert!(!replay.is_empty());
}
