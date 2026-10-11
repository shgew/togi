use std::num::NonZeroU32;

use togi_journal::{Attribution, Failure, Outcome, Payload, TrialCarried};
use togi_machine::{Condition, CoreSet, PerCore, Regime, Signal};

use super::{
    Source, carry_into, chain_of, context, core, culprit, day, hunt_start, pending, point, profile,
    resolve, seed, set, trial_end, workload,
};

const CORES: [i64; 4] = [0, 1, 2, 3];

struct Zero {
    trial: togi_journal::TrialId,
    end: togi_journal::Seq,
    failure: togi_journal::Seq,
}

fn zero_failure(source: &mut Source, failing: &[(i64, i64)]) -> Zero {
    let trial = source.next_trial();
    let mut intent = source.trial_intent(&trial, 0, 0, Condition::Parked, workload(Regime::R6, 0));
    intent.core = None;
    intent.offset = None;
    intent.cores = set(&CORES);
    intent.duration_s = 120;
    intent.activity = Some(togi_journal::Activity::Hunt);
    intent.hunt = NonZeroU32::new(1);
    intent.group = NonZeroU32::new(1);
    intent.profile = profile(failing);
    source.add(intent);
    let end = source.add(trial_end(
        &trial,
        Outcome::Failure,
        Some(Signal::Crash),
        None,
    ));
    let failure = source.add_caused(
        &[end],
        Failure {
            signal: Signal::Crash,
            attribution: Attribution::Attributed,
            core: Some(core(1)),
            offset: Some(super::offset(0)),
            trial: Some(trial.clone()),
            regime: Some(Regime::R6),
            condition: Some(Condition::Parked),
            profile: Some(profile(failing)),
            known_failure: None,
            reason: None,
            round: None,
        },
    );
    Zero {
        trial,
        end,
        failure,
    }
}

fn zero_rerun(source: &mut Source, cites: togi_journal::Seq, outcome: Outcome) {
    let trial = source.next_trial();
    let mut intent = source.trial_intent(&trial, 0, 0, Condition::Parked, workload(Regime::R6, 0));
    intent.core = None;
    intent.offset = None;
    intent.cores = set(&CORES);
    intent.duration_s = 120;
    intent.rerun = true;
    intent.profile = profile(&[(0, 0), (1, 0), (2, 0), (3, 0)]);
    source.add_caused(&[cites], intent);
    let signal = (outcome == Outcome::Failure).then_some(Signal::Crash);
    source.add(trial_end(&trial, outcome, signal, None));
}

fn seeds_of(source: &Source) -> Vec<togi_journal::CarriedCore> {
    let (mut archive, chain) = chain_of(&[source]);
    pending(&mut archive, &chain, None, &[]).seeds().to_vec()
}

#[test]
fn only_confirmed_zero_failure_points() {
    for (name, failing, rerun, confirmed) in [
        (
            "unconfirmed",
            &[(0, 0), (1, 0), (2, -20), (3, -25)],
            None,
            false,
        ),
        (
            "failing profile all at CO 0",
            &[(0, 0), (1, 0), (2, 0), (3, 0)],
            None,
            true,
        ),
        (
            "all-zero rerun failed",
            &[(0, 0), (1, 0), (2, -20), (3, -25)],
            Some(Outcome::Failure),
            true,
        ),
        (
            "all-zero rerun passed",
            &[(0, 0), (1, 0), (2, -20), (3, -25)],
            Some(Outcome::Pass),
            false,
        ),
    ] {
        let mut x = Source::new(day(9), &CORES, 9, 1).with_context(&context());
        let zero = zero_failure(&mut x, failing);
        if let Some(outcome) = rerun {
            zero_rerun(&mut x, zero.failure, outcome);
        }
        let (mut archive, chain) = chain_of(&[&x]);
        let got = pending(&mut archive, &chain, None, &[]);

        let want: Vec<_> = confirmed
            .then(|| seed(1, None, Some(point(0, x.id, zero.failure, Signal::Crash))))
            .into_iter()
            .collect();
        assert_eq!(got.seeds(), want, "{name}");
        let fact = got
            .fact_candidates()
            .first()
            .expect("the trial fact survives");
        assert_eq!(fact.source.trial.as_ref(), Some(&zero.trial), "{name}");
        assert!(fact.is_failure(), "{name}");
    }
}

#[test]
fn zero_confirmation_from_failed_trial_end_seq() {
    for (name, cites_end, outcome, confirmed) in [
        (
            "rerun cites the failed trial.end",
            true,
            Outcome::Failure,
            true,
        ),
        (
            "failed rerun cites something else",
            false,
            Outcome::Failure,
            false,
        ),
        (
            "passed rerun cites the failed trial.end",
            true,
            Outcome::Pass,
            false,
        ),
    ] {
        let mut x = Source::new(day(9), &CORES, 10, 1).with_context(&context());
        let zero = zero_failure(&mut x, &[(0, 0), (1, 0), (2, -20), (3, -25)]);
        let cites = if cites_end { zero.end } else { super::seq(1) };
        zero_rerun(&mut x, cites, outcome);
        let want: Vec<_> = confirmed
            .then(|| seed(1, None, Some(point(0, x.id, zero.failure, Signal::Crash))))
            .into_iter()
            .collect();
        assert_eq!(seeds_of(&x), want, "{name}");
    }
}

#[test]
fn zero_confirmation_keys_a_carried_failure_behind_a_culprit() {
    for (outcome, confirmed) in [(Outcome::Failure, true), (Outcome::Pass, false)] {
        let mut x = Source::new(day(9), &CORES, 10, 1).with_context(&context());
        let failing = profile(&[(0, 0), (1, 0), (2, -20), (3, -25)]);
        let carried = x.add(Payload::TrialCarried(TrialCarried {
            source: togi_journal::FactSource {
                session: day(8),
                seq: super::seq(6),
                build: super::build(9, 1, 0),
                trial: None,
                time: jiff::Timestamp::from_second(1_700_000_000).unwrap(),
                boot: togi_machine::BootId::from("boot".to_owned()),
                backend: None,
            },
            class: togi_journal::TrialClass {
                regime: Regime::R7,
                workload: Some(workload(Regime::R7, 0)),
                cores: set(&CORES),
                duration_s: 120,
            },
            condition: Condition::Together,
            activity: None,
            rerun: false,
            profile: failing,
            outcome: Outcome::Failure,
            signal: Some(Signal::Stall),
            duration_s: 30,
            core: None,
            stalled_core: None,
            voltage_requests_v: PerCore::default(),
            top_requesters: CoreSet::default(),
            ccd_mhz: std::collections::BTreeMap::new(),
        }));
        x.add(hunt_start(1, carried, failing, set(&[1])));
        let end = x.add(culprit(1, set(&[1])));
        zero_rerun(&mut x, carried, outcome);
        let want: Vec<_> = confirmed
            .then(|| seed(1, None, Some(point(0, x.id, end, Signal::Stall))))
            .into_iter()
            .collect();
        assert_eq!(seeds_of(&x), want, "{outcome:?}");
    }
}

#[test]
fn zero_proof_persisted_in_seed() {
    for (name, failing, confirmed) in [
        ("unconfirmed", &[(0, 0), (1, 0), (2, -20), (3, -25)], false),
        ("confirmed", &[(0, 0), (1, 0), (2, 0), (3, 0)], true),
    ] {
        let mut w = Source::new(day(1), &CORES, 8, 1).with_context(&context());
        let zero = zero_failure(&mut w, failing);
        let (mut archive, chain) = chain_of(&[&w]);
        let first = resolve(&pending(&mut archive, &chain, None, &[]), 1);

        let mut x = Source::new(day(2), &CORES, 9, 1).with_context(&context());
        carry_into(&mut x, &first, true);
        let (mut archive, chain) = chain_of(&[&x]);
        let second = pending(&mut archive, &chain, None, &[]);

        let want: Vec<_> = confirmed
            .then(|| seed(1, None, Some(point(0, w.id, zero.failure, Signal::Crash))))
            .into_iter()
            .collect();
        assert_eq!(second.seeds(), want, "{name}");
        assert_eq!(
            archive.opened,
            [x.id],
            "{name}: the original archive is never consulted"
        );
    }
}
