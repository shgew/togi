// Replay facts: validation, matching and the uniform choice among matches.
// Go: TestReplayDraws (the matching and uniform-choice half), TestReplayFallback (every key mismatch),
// TestReplayRequiresDeclaredContext, TestReplayRejectsInvalidEvidence (the cores of a fact are a
// sorted-unique `CoreSet`, so Go's negative and duplicate core cases cannot be written).
// TestReplayCrashTime and TestReplayCorrectedMCETime need the runtime.

use std::collections::BTreeSet;
use std::time::Duration;
use togi_machine::{BiosContext, CoreSet, Regime, Signal, TargetOffset};
use togi_sim::Load;
use togi_sim::random::{Domain, stream};
use togi_sim::replay::{FactProblem, InvalidFact, Replay, ReplayFact, ReplayOutcome};

use crate::common::{cores, offsets};

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
            "deterministic"
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
            pick(&second, &load, &profile, seed)
        );
    }
}

#[test]
fn any_difference_in_the_key_leaves_the_trial_to_the_fitted_model() {
    let replay = Replay::new([fact(ReplayOutcome::Pass)]).expect("valid");
    let regime = Regime::R7;
    let profile = offsets(&[-20, -21]);
    let base = trial(regime, "work", cores(&[0, 1]), 60);
    assert!(replay.has_answer(&context("A"), &base, &profile));

    let r2 = Regime::R2;
    let mismatches: Vec<(&str, Load<'_>, Vec<TargetOffset>, BiosContext)> = vec![
        (
            "regime",
            trial(r2, "work", cores(&[0, 1]), 60),
            profile.clone(),
            context("A"),
        ),
        (
            "workload",
            trial(regime, "other", cores(&[0, 1]), 60),
            profile.clone(),
            context("A"),
        ),
        (
            "cores",
            trial(regime, "work", cores(&[1]), 60),
            profile.clone(),
            context("A"),
        ),
        (
            "more cores",
            trial(regime, "work", cores(&[0, 1, 2]), 60),
            offsets(&[-20, -21, 0]),
            context("A"),
        ),
        (
            "idle",
            trial(regime, "work", CoreSet::default(), 60),
            profile.clone(),
            context("A"),
        ),
        (
            "duration",
            trial(regime, "work", cores(&[0, 1]), 90),
            profile.clone(),
            context("A"),
        ),
        ("loaded offset", base, offsets(&[-21, -21]), context("A")),
        ("unloaded offset", base, offsets(&[-20, -22]), context("A")),
        (
            "profile length",
            base,
            offsets(&[-20, -21, 0]),
            context("A"),
        ),
        ("context", base, profile.clone(), context("B")),
    ];
    for (name, load, profile, context) in mismatches {
        assert!(!replay.has_answer(&context, &load, &profile), "{name}");
        let mut rng = stream(1, Domain::Replay, &[]);
        assert!(
            replay.answer(&context, &load, &profile, &mut rng).is_none(),
            "{name}"
        );
    }
}

#[test]
fn condition_and_phase_are_not_part_of_the_key() {
    let replay = Replay::new([fact(ReplayOutcome::Pass)]).expect("valid");
    let regime = Regime::R7;
    let load = trial(regime, "work", cores(&[0, 1]), 60);
    assert!(replay.has_answer(&context("A"), &load, &offsets(&[-20, -21])));
}

type Edit = Box<dyn FnOnce(&mut ReplayFact)>;

fn rejected(edit: impl FnOnce(&mut ReplayFact)) -> InvalidFact {
    let mut bad = fact(failure(Signal::Stall, 7));
    edit(&mut bad);
    Replay::new([fact(ReplayOutcome::Pass), bad]).expect_err("an invalid fact")
}

#[test]
fn facts_with_invalid_evidence_are_refused_with_their_index() {
    let cases: Vec<(&str, FactProblem, Edit)> = vec![
        (
            "no context",
            FactProblem::NoContext,
            Box::new(|f| f.context = BiosContext::default()),
        ),
        (
            "no cores",
            FactProblem::NoCores,
            Box::new(|f| f.cores = CoreSet::default()),
        ),
        (
            "a core outside the profile",
            FactProblem::CoreOutsideProfile { core: 2, cores: 2 },
            Box::new(|f| f.cores = cores(&[0, 2])),
        ),
        (
            "no duration",
            FactProblem::NoDuration,
            Box::new(|f| f.duration = Duration::ZERO),
        ),
        (
            "an exposure beyond the duration",
            FactProblem::ExposureTooLong,
            Box::new(|f| f.outcome = failure(Signal::Stall, 61)),
        ),
    ];
    for (name, problem, edit) in cases {
        let error = rejected(edit);
        assert_eq!((error.index, error.problem), (1, problem), "{name}");
    }
}

#[test]
fn every_failure_signal_the_extract_can_hold_is_accepted_and_a_zero_exposure_is_valid() {
    for signal in [
        Signal::ComputationError,
        Signal::Stall,
        Signal::UnexpectedExit,
        Signal::CorrectedMce,
        Signal::UncorrectedMce,
        Signal::Crash,
    ] {
        assert!(Replay::new([fact(failure(signal, 0))]).is_ok(), "{signal}");
    }
    assert!(
        Replay::new([fact(failure(Signal::Crash, 60))]).is_ok(),
        "exposure equals duration"
    );
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
