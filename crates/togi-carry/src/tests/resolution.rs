use std::collections::HashSet;
use std::path::PathBuf;

use togi_config::Backends;
use togi_journal::Attribution;
use togi_machine::{Backend, Condition, Regime};

use super::{
    Source, carry_into, chain_of, context, day, idle_failure, ids, pending, profile, resolve, set,
    workload,
};
use crate::copied_identity;

fn backends(mprime: &str, ycruncher: &str) -> Backends {
    Backends::new(Some(PathBuf::from(mprime)), Some(PathBuf::from(ycruncher))).unwrap()
}

const OLD_MPRIME: &str = "/store/0000-mprime-31.04b02";
const OLD_YCRUNCHER: &str = "/store/0000-y-cruncher-0.8.7.9547";
const NEW_MPRIME: &str = "/store/0001-mprime-31.04b03";
const NEW_YCRUNCHER: &str = "/store/0002-y-cruncher-0.8.7.9548";

#[test]
fn resolve_drops_only_changed_backend_passes_keeps_failures() {
    let mprime = workload(Regime::R1, 0);
    let ycruncher = workload(Regime::R1, 1);
    assert_eq!(mprime.backend(), Backend::Mprime);
    assert_eq!(ycruncher.backend(), Backend::Ycruncher);

    let mut a = Source::new(day(1), &[0, 1], 3, 1).with_context(&context());
    a.backends(OLD_MPRIME, OLD_YCRUNCHER);
    let mprime_failure = a
        .fail_on(0, -30, Condition::Alone, Attribution::Attributed, mprime)
        .end;
    let mprime_pass = a.pass_on(0, -30, Condition::Alone, mprime);
    let ycruncher_failure = a
        .fail_on(0, -30, Condition::Alone, Attribution::Attributed, ycruncher)
        .end;
    let ycruncher_pass = a.pass_on(0, -30, Condition::Alone, ycruncher);
    let (mut archive, chain) = chain_of(&[&a]);
    let got = pending(&mut archive, &chain, None, &[]);

    for (name, current, keep_mprime, keep_ycruncher) in [
        (
            "same backends",
            backends(OLD_MPRIME, OLD_YCRUNCHER),
            true,
            true,
        ),
        (
            "mprime updated between sessions",
            backends(NEW_MPRIME, OLD_YCRUNCHER),
            false,
            true,
        ),
        (
            "y-cruncher updated between sessions",
            backends(OLD_MPRIME, NEW_YCRUNCHER),
            true,
            false,
        ),
    ] {
        let resolved = got.resolve(&context(), &current, 1);
        let mut want = vec![a.id_of(mprime_failure)];
        if keep_mprime {
            want.push(a.id_of(mprime_pass));
        }
        want.push(a.id_of(ycruncher_failure));
        if keep_ycruncher {
            want.push(a.id_of(ycruncher_pass));
        }
        assert_eq!(ids(resolved.facts()), want, "{name}");
    }
    assert_eq!(
        ids(got.fact_candidates()).len(),
        4,
        "a later different backend cannot delete passes from the pending carry"
    );
}

#[test]
fn copied_pass_keeps_original_backend() {
    let mprime = workload(Regime::R1, 0);
    let mut a = Source::new(day(1), &[0, 1], 6, 1).with_context(&context());
    a.backends(OLD_MPRIME, OLD_YCRUNCHER);
    let a_pass = a.pass_on(0, -30, Condition::Alone, mprime);
    let (mut archive, chain) = chain_of(&[&a]);
    let first = pending(&mut archive, &chain, None, &[]).resolve(
        &context(),
        &backends(OLD_MPRIME, OLD_YCRUNCHER),
        1,
    );
    assert_eq!(first.facts().len(), 1);

    let mut b = Source::new(day(2), &[0, 1], 6, 1).with_context(&context());
    b.backends(NEW_MPRIME, OLD_YCRUNCHER);
    carry_into(&mut b, &first, true);
    let b_pass = b.pass_on(0, -30, Condition::Alone, mprime);

    let (mut archive, chain) = chain_of(&[&b, &a]);
    let got = pending(&mut archive, &chain, None, &[]);
    let under_new = got.resolve(&context(), &backends(NEW_MPRIME, OLD_YCRUNCHER), 1);
    assert_eq!(ids(under_new.facts()), [b.id_of(b_pass)]);
    let under_old = got.resolve(&context(), &backends(OLD_MPRIME, OLD_YCRUNCHER), 1);
    assert_eq!(ids(under_old.facts()), [a.id_of(a_pass)]);
}

#[test]
fn resolve_idempotent() {
    let mut a = Source::new(day(1), &[0, 1], 6, 1).with_context(&context());
    a.pass(0, -30, Condition::Alone);
    a.fail(1, -30, Condition::Alone, Attribution::Attributed);
    let (mut archive, chain) = chain_of(&[&a]);
    let got = pending(&mut archive, &chain, None, &[]);
    let before = got.clone();
    let first = resolve(&got, 1);
    let second = resolve(&got, 1);
    assert_eq!(first, second);
    assert_eq!(got, before, "resolution never changes the pending carry");
    assert_eq!(
        first.session_carried(set(&[0, 1])),
        second.session_carried(set(&[0, 1]))
    );
}

#[test]
fn emission_filters_to_machine_cores_and_missing_copies() {
    let mut a = Source::new(day(1), &[0, 1, 2], 6, 1).with_context(&context());
    let pass = a.pass(0, -30, Condition::Alone);
    a.fail(1, -30, Condition::Alone, Attribution::Attributed);
    a.add(idle_failure(profile(&[(0, -30), (1, -30), (2, -30)])));
    let (mut archive, chain) = chain_of(&[&a]);
    let resolved = resolve(&pending(&mut archive, &chain, None, &[]), 1);
    assert_eq!(resolved.facts().len(), 3);

    let machine = set(&[0, 1]);
    let carried = resolved.session_carried(machine);
    assert!(
        carried
            .carried
            .iter()
            .all(|seed| machine.contains(seed.core))
    );
    assert_eq!(carried.sources, resolved.sources());

    let none = HashSet::new();
    assert_eq!(
        resolved.fact_payloads(&none, machine).len(),
        2,
        "the idle failure loads core 2, absent from this machine"
    );

    let copied: HashSet<_> = [a.id_of(pass)].into();
    let payloads = resolved.fact_payloads(&copied, set(&[0, 1, 2]));
    assert_eq!(payloads.len(), 2);
    assert!(
        payloads
            .iter()
            .all(|payload| { copied_identity(payload).is_some_and(|id| id != a.id_of(pass)) })
    );
}
