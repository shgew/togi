use std::num::NonZeroU32;

use togi_facts::{Decisive, Fact, FactId, Observation};
use togi_journal::{
    Attribution, CarriedSource, Failure, FailureCarried, Outcome, Payload, ResetTarget,
    SessionCarried, TrialCarried,
};
use togi_machine::{CcdId, Condition, Regime, Signal};

use super::{
    Archive, DEFECTS, Source, backoff_decision, before, carry_into, chain_of, context, core,
    culprit, day, hunt_start, idle_failure, ids, other_context, pending, profile, reset, resolve,
    session, set, trial_end, workload,
};

use crate::{CarryError, select};

fn without_local(fact: &Fact) -> Fact {
    Fact {
        local: togi_journal::Seq::FIRST,
        ..fact.clone()
    }
}

fn first_transition(sources: &[&Source], epoch: u32) -> crate::ResolvedCarry {
    let (mut archive, chain) = chain_of(sources);
    resolve(&pending(&mut archive, &chain, None, &[]), epoch)
}

#[test]
fn fact_eligibility_across_archive_chain() {
    for (name, epoch, bios_boundary, new_bios, reset_all, reset_core) in [
        ("reset epochs and defects", 1, false, false, false, false),
        ("evidence epoch bump", 2, false, false, false, false),
        ("BIOS archive boundary", 1, true, false, false, false),
        (
            "new BIOS carries only candidate solo limits",
            1,
            false,
            true,
            false,
            false,
        ),
        ("newest reset all", 1, false, false, true, false),
        ("newest core reset", 1, false, false, false, true),
    ] {
        let mut a = Source::new(day(1), &[0, 1], 5, 1).with_context(&context());
        a.fail(1, -30, Condition::Alone, Attribution::Attributed);
        a.add(reset(ResetTarget::All));
        let a_failure = a
            .fail(1, -30, Condition::Alone, Attribution::Attributed)
            .end;
        let a_pass = a.pass(1, -30, Condition::Alone);

        let b_context = if bios_boundary {
            other_context()
        } else {
            context()
        };
        let mut b = Source::new(day(2), &[0, 1], 6, 1).with_context(&b_context);
        b.fail(0, -30, Condition::Alone, Attribution::Attributed);
        b.add(reset(ResetTarget::Core(core(0))));
        let b_failure = b
            .fail(0, -30, Condition::Alone, Attribution::Attributed)
            .end;
        let b_pass = b.pass(1, -30, Condition::Alone);
        let excluded = b.fail(1, -30, Condition::Alone, Attribution::Attributed);
        b.add_caused(&[excluded.failure], backoff_decision(1));
        b.fixes(1);
        let fixed = b.fail(1, -30, Condition::Alone, Attribution::Attributed);
        b.add_caused(&[fixed.failure], backoff_decision(1));

        let mut c = Source::new(day(3), &[0, 1], 6, 1).with_context(&context());
        if reset_all {
            c.add(reset(ResetTarget::All));
        }
        if reset_core {
            c.add(reset(ResetTarget::Core(core(1))));
        }
        let c_pass = c.pass(0, -30, Condition::Alone);
        let c_failure = c
            .fail(1, -30, Condition::Alone, Attribution::Attributed)
            .end;

        let (mut archive, chain) = chain_of(&[&c, &b, &a]);
        let got = pending(&mut archive, &chain, None, &DEFECTS);
        let current = if new_bios { other_context() } else { context() };
        let resolved = got.resolve(&current, &togi_config::Backends::default(), epoch);

        let mut want = Vec::new();
        if !new_bios {
            if !bios_boundary && !reset_all {
                if !reset_core {
                    want.push(a.id_of(a_failure));
                    if epoch == 1 {
                        want.push(a.id_of(a_pass));
                    }
                }
                want.push(b.id_of(b_failure));
                if !reset_core {
                    if epoch == 1 {
                        want.push(b.id_of(b_pass));
                    }
                    want.push(b.id_of(fixed.end));
                }
            }
            if epoch == 1 {
                want.push(c.id_of(c_pass));
            }
            want.push(c.id_of(c_failure));
        }
        assert_eq!(ids(resolved.facts()), want, "{name}");
    }
}

#[test]
fn fact_copy_forward_preserves_provenance_and_stops_walk() {
    for epoch in [1, 2] {
        let mut a = Source::new(day(1), &[0, 1], 6, 1).with_context(&context());
        let a_pass = a.pass(0, -30, Condition::Alone);
        let a_failure = a
            .fail(1, -30, Condition::Alone, Attribution::Attributed)
            .end;
        let first = first_transition(&[&a], 1);
        assert_eq!(first.facts().len(), 2);

        let mut b = Source::new(day(2), &[0, 1], 6, 1).with_context(&context());
        carry_into(&mut b, &first, true);
        let b_own = b
            .fail(0, -30, Condition::Alone, Attribution::Attributed)
            .end;

        let (mut archive, chain) = chain_of(&[&b]);
        let chain = [chain, vec![a.id]].concat();
        archive.poison(a.id);
        let second = resolve(&pending(&mut archive, &chain, None, &[]), epoch);

        let mut want = vec![a.id_of(a_failure), b.id_of(b_own)];
        if epoch == 1 {
            want.insert(0, a.id_of(a_pass));
        }
        assert_eq!(ids(second.facts()), want, "epoch {epoch}");
        for copy in second
            .facts()
            .iter()
            .filter(|fact| fact.source.session == a.id)
        {
            let original = first
                .facts()
                .iter()
                .find(|fact| fact.id() == copy.id())
                .unwrap();
            assert_eq!(without_local(copy), without_local(original));
        }
    }
}

struct Recheck {
    a: Source,
    b: Source,
    a_pass: FactId,
    a_failure: FactId,
    idle: FactId,
    b_own: FactId,
}

fn recheck(a_fixed: bool) -> Recheck {
    let mut z = Source::new(before(), &[0, 1], 6, 1).with_context(&context());
    z.fail(0, -30, Condition::Alone, Attribution::Attributed);
    let mut a = Source::new(day(1), &[0, 1], 6, 1).with_context(&context());
    if a_fixed {
        a.fixes(1);
    }
    let a_pass = a.pass(0, -30, Condition::Alone);
    let failed = a.fail(1, -30, Condition::Alone, Attribution::Attributed);
    a.add_caused(&[failed.failure], backoff_decision(1));
    let idle = a.add(idle_failure(profile(&[(0, -30), (1, -30)])));
    a.add_caused(&[idle], backoff_decision(0));

    let (mut archive, chain) = chain_of(&[&a, &z]);
    let first = resolve(&pending(&mut archive, &chain, None, &[]), 1);
    let own: Vec<_> = first
        .facts()
        .iter()
        .filter(|fact| fact.source.session == a.id)
        .cloned()
        .collect();
    assert_eq!(own.len(), 3);

    let mut b = Source::new(day(2), &[0, 1], 6, 1).with_context(&context());
    b.fixes(1);
    for fact in &own {
        b.add(fact.to_payload());
    }
    b.add(SessionCarried {
        sources: vec![CarriedSource {
            session: a.id,
            schema: togi_journal::SCHEMA,
            ruleset: 6,
        }],
        failure_points: true,
        detail: None,
        carried: Vec::new(),
    });
    let b_own = b
        .fail(0, -30, Condition::Alone, Attribution::Attributed)
        .end;
    Recheck {
        a_pass: a.id_of(a_pass),
        a_failure: a.id_of(failed.end),
        idle: a.id_of(idle),
        b_own: b.id_of(b_own),
        a,
        b,
    }
}

#[test]
fn copied_failures_rechecked_against_original_defects() {
    for (name, fixed, retained) in [
        (
            "new defect drops original trial and idle failures",
            false,
            false,
        ),
        ("original fixes stamp protects failures", true, true),
    ] {
        let r = recheck(fixed);
        let (mut archive, _) = chain_of(&[&r.b, &r.a]);
        let chain = [r.b.id, r.a.id];
        let got = resolve(&pending(&mut archive, &chain, None, &DEFECTS), 1);
        let mut want = vec![r.a_pass];
        if retained {
            want.extend([r.a_failure, r.idle]);
        }
        want.push(r.b_own);
        assert_eq!(ids(got.facts()), want, "{name}");
        assert_eq!(
            archive.opened,
            [r.b.id, r.a.id],
            "{name}: older evidence is not collected, the original only rechecked"
        );
    }
}

#[test]
fn copied_failures_without_the_original_archive_are_retained() {
    let r = recheck(false);
    let (mut archive, chain) = chain_of(&[&r.b]);
    let got = resolve(&pending(&mut archive, &chain, None, &DEFECTS), 1);
    assert_eq!(ids(got.facts()), [r.a_pass, r.a_failure, r.idle, r.b_own]);
    assert_eq!(archive.opened, [r.b.id, r.a.id]);
}

#[test]
fn copied_failures_with_an_unreadable_original_stop_carry() {
    let r = recheck(false);
    let (mut archive, chain) = chain_of(&[&r.b]);
    archive.corrupt(r.a.id);
    let got = select(&chain, None, &DEFECTS, &mut archive);
    assert!(matches!(
        got,
        Err(CarryError::Read { session, .. }) if session == r.a.id
    ));
}

#[test]
fn empty_registry_skips_original_lookup() {
    let r = recheck(false);
    let (mut archive, chain) = chain_of(&[&r.b]);
    archive.corrupt(r.a.id);
    let got = resolve(&pending(&mut archive, &chain, None, &[]), 1);
    assert_eq!(got.facts().len(), 4);
    assert_eq!(archive.opened, [r.b.id]);
}

#[test]
fn resolve_requires_context_and_retries() {
    let a = Source::new(day(1), &[0, 1], 3, 1).with_context(&context());
    let mut older = Source::new(before(), &[0, 1], 2, 1).with_context(&context());
    let key = older
        .fail(0, -30, Condition::Alone, Attribution::Attributed)
        .end;
    let chain = [a.id, older.id];

    let mut broken = Archive::of(&[&a]);
    broken.corrupt(older.id);
    assert!(select(&chain, None, &[], &mut broken).is_err());

    let mut healthy = Archive::of(&[&a, &older]);
    let got = pending(&mut healthy, &chain, None, &[]);
    let resolved = resolve(&got, 1);
    assert_eq!(ids(resolved.facts()), [older.id_of(key)]);
    assert_eq!(resolve(&got, 1), resolved, "resolution reads nothing");

    let elsewhere = got.resolve(&other_context(), &togi_config::Backends::default(), 1);
    assert!(elsewhere.facts().is_empty());
}

#[test]
fn corrupt_older_source_is_error() {
    let a = Source::new(day(1), &[0], 2, 1).with_context(&context());
    let b = Source::new(day(2), &[0], 3, 1).with_context(&context());
    let (mut archive, chain) = chain_of(&[&b]);
    archive.corrupt(a.id);
    let chain = [chain, vec![a.id]].concat();
    let got = select(&chain, None, &[], &mut archive);
    assert!(matches!(got, Err(CarryError::Read { session, .. }) if session == a.id));
}

#[test]
fn missing_listed_source_is_error() {
    let b = Source::new(day(2), &[0], 3, 1).with_context(&context());
    let (mut archive, _) = chain_of(&[&b]);
    let got = select(&[b.id, day(1)], None, &[], &mut archive);
    assert!(matches!(got, Err(CarryError::Missing { session }) if session == day(1)));
}

fn telemetry_failure(source: &mut Source, recorded: bool) -> togi_journal::Seq {
    let trial = source.next_trial();
    let mut intent =
        source.trial_intent(&trial, 0, -30, Condition::Together, workload(Regime::R7, 0));
    intent.core = None;
    intent.offset = None;
    intent.cores = set(&[1, 0]);
    intent.duration_s = 120;
    source.add(intent);
    let mut end = trial_end(&trial, Outcome::Failure, Some(Signal::Stall), None);
    end.stalled_core = Some(core(0));
    end.duration_s = 30;
    if recorded {
        end.voltage_requests_v = [(core(0), 1.125), (core(1), 1.25)].into_iter().collect();
        end.top_requesters = set(&[0, 1]);
        end.ccd_mhz = [(CcdId(4), 4800), (CcdId(9), 4900)].into_iter().collect();
    }
    source.add(end)
}

#[test]
fn copied_telemetry_dedup_prefix() {
    for recorded in [true, false] {
        let mut a = Source::new(day(1), &[0, 1], 8, 1).with_context(&context());
        telemetry_failure(&mut a, recorded);
        let first = first_transition(&[&a], 1);
        assert_eq!(first.facts().len(), 1);
        let Observation::Trial {
            telemetry, outcome, ..
        } = &first.facts()[0].observation
        else {
            panic!("not a trial fact");
        };
        assert_eq!(telemetry.is_some(), recorded);
        assert!(matches!(
            outcome,
            Decisive::Failure { stalled: Some(stalled), .. } if *stalled == core(0)
        ));

        let mut b = Source::new(day(2), &[0, 1], 8, 1).with_context(&context());
        carry_into(&mut b, &first, false);
        let (mut archive, chain) = chain_of(&[&b, &a]);
        let resumed = resolve(&pending(&mut archive, &chain, None, &[]), 1);
        assert_eq!(
            resumed.facts().len(),
            1,
            "an uncommitted prefix does not duplicate"
        );
        assert_eq!(
            without_local(&resumed.facts()[0]),
            without_local(&first.facts()[0])
        );
    }
}

#[test]
fn same_epoch_transition_retains_bios_evidence() {
    let mut s = Source::new(day(1), &[0, 1], 7, 1).with_context(&context());
    let pass = s.pass(0, -30, Condition::Alone);
    let failed = s.fail(1, -30, Condition::Alone, Attribution::Attributed);
    let (mut archive, chain) = chain_of(&[&s]);
    let got = pending(&mut archive, &chain, None, &[]);

    let same = resolve(&got, 1);
    assert_eq!(same.sources().len(), 1);
    assert_eq!(ids(same.facts()), [s.id_of(pass), s.id_of(failed.end)]);
    assert_eq!(same.seeds().len(), 2);

    let bumped = resolve(&got, 2);
    assert_eq!(ids(bumped.facts()), [s.id_of(failed.end)]);
    assert_eq!(
        bumped.seeds(),
        same.seeds(),
        "seeds do not depend on the epoch"
    );
}

#[test]
fn interrupted_epoch_transition_retains_original_seeds_and_failures() {
    for source_evidence in [0, 1] {
        let mut a = Source::new(day(1), &[0, 1], 6, source_evidence).with_context(&context());
        a.pass(0, -30, Condition::Alone);
        let failed = a.fail(1, -30, Condition::Alone, Attribution::Attributed);
        let (mut archive, chain) = chain_of(&[&a]);
        let first = resolve(&pending(&mut archive, &chain, None, &[]), 2);
        assert_eq!(first.seeds().len(), 2);
        assert_eq!(ids(first.facts()), [a.id_of(failed.end)]);

        let p = Source::new(day(2), &[0, 1], 6, 2).with_context(&context());
        let (mut archive, chain) = chain_of(&[&p, &a]);
        let second_pending = pending(&mut archive, &chain, None, &[]);
        let second = resolve(&second_pending, 3);
        assert_eq!(second.seeds(), first.seeds());
        assert_eq!(second.facts(), first.facts());
        assert_eq!(
            second_pending
                .sources()
                .iter()
                .map(|s| s.session)
                .collect::<Vec<_>>(),
            [p.id, a.id]
        );
    }
}

#[test]
fn reset_of_loaded_core_filters_copied_facts() {
    for all in [false, true] {
        let mut a = Source::new(day(1), &[0, 1], 6, 1).with_context(&context());
        a.pass(0, -30, Condition::Alone);
        let a_core1 = a
            .fail(1, -30, Condition::Alone, Attribution::Attributed)
            .end;
        a.add_caused(&[seq_of(2)], idle_failure(profile(&[(0, -30), (1, -30)])));
        let copied = first_transition(&[&a], 1);
        assert_eq!(copied.facts().len(), 3);

        let mut b = Source::new(day(2), &[0, 1], 6, 1).with_context(&context());
        carry_into(&mut b, &copied, false);
        b.add(reset(if all {
            ResetTarget::All
        } else {
            ResetTarget::Core(core(0))
        }));
        let own = b.pass(0, -30, Condition::Alone);

        let (mut archive, chain) = chain_of(&[&b, &a]);
        let got = resolve(&pending(&mut archive, &chain, None, &[]), 1);
        let mut want = vec![b.id_of(own)];
        if !all {
            want.insert(0, a.id_of(a_core1));
        }
        assert_eq!(ids(got.facts()), want, "reset all {all}");
    }
}

fn seq_of(n: u64) -> togi_journal::Seq {
    super::seq(n)
}

#[test]
fn facts_sorted_by_session_id_then_seq() {
    let ids_in_order = [
        session("20261001T000000Z"),
        session("20261001T000000Z-2"),
        session("20261001T000000Z-10"),
    ];
    let mut sources: Vec<Source> = ids_in_order
        .iter()
        .enumerate()
        .map(|(n, &id)| {
            let mut source =
                Source::new(id, &[0], u32::try_from(n).unwrap() + 1, 1).with_context(&context());
            source.fail(0, -30, Condition::Alone, Attribution::Attributed);
            source
        })
        .collect();
    sources.reverse();
    let (mut archive, chain) = chain_of(&sources.iter().collect::<Vec<_>>());
    let got = resolve(&pending(&mut archive, &chain, None, &[]), 1);
    let sessions: Vec<_> = got.facts().iter().map(|fact| fact.source.session).collect();
    assert_eq!(sessions, ids_in_order);
}

#[test]
fn culprit_from_known_failure_keeps_source_signal() {
    for kind in ["live trial", "carried trial", "carried idle"] {
        for reset_after in [false, true] {
            let mut x = Source::new(day(9), &[5, 0, 2], 7, 1).with_context(&context());
            x.add(SessionCarried {
                sources: vec![CarriedSource {
                    session: day(8),
                    schema: togi_journal::SCHEMA,
                    ruleset: 6,
                }],
                failure_points: true,
                detail: None,
                carried: Vec::new(),
            });
            let failing = profile(&[(0, -10), (2, -35), (5, -20)]);
            let original = togi_journal::FactSource {
                session: day(8),
                seq: super::seq(4),
                build: super::build(6, 1, 0),
                trial: None,
                time: jiff::Timestamp::from_second(1_700_000_000).unwrap(),
                boot: togi_machine::BootId::from("boot".to_owned()),
                backend: None,
            };
            let class = togi_journal::TrialClass {
                regime: Regime::R7,
                workload: Some(workload(Regime::R7, 0)),
                cores: set(&[0, 2, 5]),
                duration_s: 120,
            };
            let (failure, signal) = match kind {
                "live trial" => {
                    let trial = x.next_trial();
                    let mut intent = x.trial_intent(
                        &trial,
                        0,
                        -10,
                        Condition::Together,
                        workload(Regime::R7, 0),
                    );
                    intent.core = None;
                    intent.offset = None;
                    intent.cores = set(&[0, 2, 5]);
                    intent.profile = failing;
                    x.add(intent);
                    let end = x.add(trial_end(
                        &trial,
                        Outcome::Failure,
                        Some(Signal::ComputationError),
                        None,
                    ));
                    (end, Signal::ComputationError)
                }
                "carried trial" => {
                    let seq = x.add(Payload::TrialCarried(TrialCarried {
                        source: togi_journal::FactSource {
                            trial: Some(super::TrialId::from("origin".to_owned())),
                            ..original
                        },
                        class,
                        condition: Condition::Together,
                        activity: None,
                        rerun: false,
                        profile: failing,
                        outcome: Outcome::Failure,
                        signal: Some(Signal::Stall),
                        duration_s: 30,
                        core: None,
                        stalled_core: None,
                        voltage_requests_v: togi_machine::PerCore::default(),
                        top_requesters: togi_machine::CoreSet::default(),
                        ccd_mhz: std::collections::BTreeMap::new(),
                    }));
                    (seq, Signal::Stall)
                }
                _ => {
                    let seq = x.add(Payload::FailureCarried(FailureCarried {
                        source: original,
                        class: togi_journal::TrialClass {
                            regime: Regime::R6,
                            workload: None,
                            cores: set(&[0, 2, 5]),
                            duration_s: 0,
                        },
                        failure: Failure {
                            signal: Signal::Crash,
                            attribution: Attribution::Unattributed,
                            core: None,
                            offset: None,
                            trial: None,
                            regime: Some(Regime::R6),
                            condition: Some(Condition::Together),
                            profile: Some(failing),
                            known_failure: None,
                            reason: None,
                            round: None,
                        },
                    }));
                    (seq, Signal::Crash)
                }
            };
            let skip = x.add_caused(
                &[failure],
                Failure {
                    signal,
                    attribution: Attribution::Unattributed,
                    core: None,
                    offset: None,
                    trial: None,
                    regime: Some(Regime::R7),
                    condition: Some(Condition::Together),
                    profile: Some(failing),
                    known_failure: Some(failure),
                    reason: None,
                    round: None,
                },
            );
            let mut start = hunt_start(1, failure, failing, set(&[2]));
            start.hunt = NonZeroU32::new(1).unwrap();
            x.add_caused(&[failure, skip], start);
            let end = x.add(culprit(1, set(&[2])));
            if reset_after {
                x.add(reset(ResetTarget::Core(core(2))));
            }

            let (mut archive, chain) = chain_of(&[&x]);
            let got = pending(&mut archive, &chain, None, &[]);
            let want: Vec<_> = (!reset_after)
                .then(|| super::seed(2, None, Some(super::point(-35, x.id, end, signal))))
                .into_iter()
                .collect();
            assert_eq!(got.seeds(), want, "{kind} reset {reset_after}");
        }
    }
}

#[test]
fn original_outside_the_walk_is_rechecked_when_present() {
    for (fixed, retained) in [(false, false), (true, true)] {
        let r = recheck(fixed);
        let (mut archive, _) = chain_of(&[&r.b, &r.a]);
        let got = resolve(&pending(&mut archive, &[r.b.id], None, &DEFECTS), 1);
        let mut want = vec![r.a_pass];
        if retained {
            want.extend([r.a_failure, r.idle]);
        }
        want.push(r.b_own);
        assert_eq!(ids(got.facts()), want, "original fixed {fixed}");
        assert_eq!(archive.opened, [r.b.id, r.a.id]);
    }
}

#[test]
fn an_original_that_cannot_be_opened_stops_carry() {
    let r = recheck(false);
    let (mut archive, chain) = chain_of(&[&r.b]);
    archive.unopenable(r.a.id);
    let got = select(&chain, None, &DEFECTS, &mut archive);
    assert!(matches!(
        got,
        Err(CarryError::Open { session, .. }) if session == r.a.id
    ));
}

#[test]
fn an_empty_chain_carries_nothing() {
    let mut archive = Archive::default();
    assert!(select(&[], None, &[], &mut archive).unwrap().is_none());
    assert!(archive.opened.is_empty());
}
