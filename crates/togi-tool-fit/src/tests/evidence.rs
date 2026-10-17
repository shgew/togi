use togi_facts::Decisive;
use togi_machine::Regime;
use togi_tool_trialfacts::Class;

use super::{context, crash, fact, refs, repeated};
use crate::error::{EvidenceError, Problem};
use crate::evidence::{aggregate, bootstrap, decisive, initial_config};

fn valid() -> togi_tool_trialfacts::TrialFact {
    let mut fact = fact(&[-50, 0], Regime::R1, &[0], 60, Decisive::Pass);
    fact.profile = super::profile(&[-50, 0]);
    fact
}

#[test]
fn a_fact_that_describes_no_machine_exposure_is_refused() {
    type Change = fn(&mut togi_tool_trialfacts::TrialFact);
    let cases: [(&str, Change, Problem); 5] = [
        (
            "one core",
            |f| f.profile = super::profile(&[0]),
            Problem::CoreCount(1),
        ),
        (
            "odd cores",
            |f| f.profile = super::profile(&[0, 0, 0]),
            Problem::CoreCount(3),
        ),
        (
            "no loaded cores",
            |f| {
                if let Class::Trial(class) = &mut f.class {
                    class.loaded = super::cores(&[]);
                }
            },
            Problem::NoLoadedCore,
        ),
        (
            "no exposure",
            |f| {
                if let Class::Trial(class) = &mut f.class {
                    class.duration_s = 0;
                }
            },
            Problem::ZeroDuration,
        ),
        (
            "past the last core",
            |f| {
                if let Class::Trial(class) = &mut f.class {
                    class.loaded = super::cores(&[2]);
                }
            },
            Problem::LoadedCore { core: 2, cores: 2 },
        ),
    ];
    for (name, change, problem) in cases {
        let mut fact = valid();
        change(&mut fact);
        let error = decisive(&[fact.clone()]).unwrap_err();
        assert_eq!(
            error,
            EvidenceError::Invalid {
                session: fact.session,
                seq: fact.seq,
                problem
            },
            "{name}"
        );
    }
    let mut mismatch = valid();
    mismatch.profile = super::profile(&[0, 0, 0, 0]);
    assert_eq!(
        decisive(&[valid(), mismatch]).unwrap_err().to_string(),
        format!(
            "invalid trial fact {}:1: the profile has 4 offsets, the first fact's has 2",
            super::SESSION
        )
    );
}

#[test]
fn idle_failures_are_left_out_and_the_context_is_kept() {
    let trial = fact(&[-50, 0], Regime::R1, &[0], 60, crash());
    let mut idle = trial.clone();
    idle.class = Class::Idle {
        loaded: super::cores(&[0]),
    };
    let records = [idle.clone(), trial.clone()];
    let got = decisive(&records).unwrap();
    assert_eq!(got, [&trial]);
    let cfg = initial_config(&got);
    let bios = cfg.bios_context.unwrap();
    assert_eq!(bios.board.as_deref(), Some(context().board.as_str()));
    assert_eq!(bios.boost_limit_mhz, Some(5600));
    assert_eq!(decisive(&[]), Err(EvidenceError::NoTrials));
    assert_eq!(decisive(&[idle]), Err(EvidenceError::NoTrials));
}

#[test]
fn evidence_of_two_bios_contexts_is_refused_whichever_comes_first() {
    let known = valid();
    let mut other = valid();
    other.context.as_mut().unwrap().bios_version = "B".into();
    let mut missing = valid();
    missing.context = None;
    for pair in [[&known, &other], [&known, &missing], [&missing, &known]] {
        let records = [pair[0].clone(), pair[1].clone()];
        assert_eq!(decisive(&records), Err(EvidenceError::MixedContexts));
    }
}

#[test]
fn the_same_seed_resamples_the_same_trials_and_another_seed_others() {
    let template = fact(&[-10, 0], Regime::R1, &[0], 60, Decisive::Pass);
    let records = repeated(&template, 40, 10);
    let records = refs(&records);
    assert_eq!(bootstrap(&records, 263), bootstrap(&records, 263));
    assert_ne!(bootstrap(&records, 263), bootstrap(&records, 264));
    assert_eq!(bootstrap(&records, 263).len(), 40);
}

#[test]
fn equal_profiles_and_classes_aggregate_with_their_counts_in_first_seen_order() {
    let a = fact(&[-10, 0], Regime::R1, &[0], 60, Decisive::Pass);
    let b = fact(&[-20, 0], Regime::R1, &[0], 60, Decisive::Pass);
    let mut records = repeated(&a, 5, 2);
    records.extend(repeated(&b, 3, 1));
    records.extend(repeated(&a, 2, 2));
    let got: Vec<_> = aggregate(&refs(&records))
        .iter()
        .map(|o| (o.profile.as_slice()[0].get(), o.n, o.k))
        .collect();
    assert_eq!(got, [(-10, 7, 4), (-20, 3, 1)]);
}

mod properties {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, RngSeed};
    use togi_facts::Decisive;
    use togi_machine::Regime;

    use crate::evidence::{aggregate, bootstrap};
    use crate::forward::log_loss;
    use crate::tests::{crash, fact, refs};

    fn config() -> Config {
        Config {
            cases: 64,
            rng_seed: RngSeed::Fixed(0x6669_745f_7072_6f70),
            failure_persistence: None,
            ..Config::default()
        }
    }

    proptest! {
        #![proptest_config(config())]

        #[test]
        fn aggregating_keeps_every_trial_and_failure(
            trials in proptest::collection::vec((0_i64..4, any::<bool>()), 1..60)
        ) {
            let records: Vec<_> = trials
                .iter()
                .map(|&(depth, failing)| {
                    let outcome = if failing { crash() } else { Decisive::Pass };
                    fact(&[-10 * depth, 0], Regime::R1, &[0], 60, outcome)
                })
                .collect();
            let observations = aggregate(&refs(&records));
            let n: u32 = observations.iter().map(|o| o.n).sum();
            let k: u32 = observations.iter().map(|o| o.k).sum();
            prop_assert_eq!(usize::try_from(n).unwrap(), records.len());
            prop_assert_eq!(usize::try_from(k).unwrap(), trials.iter().filter(|t| t.1).count());
            prop_assert!(observations.iter().all(|o| o.k <= o.n));
        }

        #[test]
        fn a_resample_is_as_long_as_its_source_and_draws_only_from_it(seed in any::<u64>(), len in 1_usize..30) {
            let records: Vec<_> = (0..len)
                .map(|i| fact(&[-i64::try_from(i).unwrap(), 0], Regime::R1, &[0], 60, Decisive::Pass))
                .collect();
            let records = refs(&records);
            let sample = bootstrap(&records, seed);
            prop_assert_eq!(sample.len(), records.len());
            prop_assert!(sample.iter().all(|s| records.contains(s)));
            prop_assert_eq!(bootstrap(&records, seed), sample);
        }

        #[test]
        fn the_clamped_log_loss_is_finite_and_never_negative(p in 0.0_f64..=1.0, failure in any::<bool>()) {
            let loss = log_loss(p, failure);
            prop_assert!(loss.is_finite() && loss > 0.0);
        }
    }
}
