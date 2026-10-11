use togi_journal::Attribution;
use togi_machine::{Condition, Signal};

use super::{
    Archive, Source, before, carry_into, chain_of, context, day, ids, limit, pending, point,
    resolve, seed, select,
};
use togi_journal::SessionCarried;

#[test]
fn fact_prefix_needs_commitment_to_stop_walk() {
    for committed in [false, true] {
        let mut a = Source::new(day(1), &[0, 1], 6, 1).with_context(&context());
        let a_pass = a.pass(0, -30, Condition::Alone);
        let a_failure = a
            .fail(1, -30, Condition::Alone, Attribution::Attributed)
            .end;
        let (mut archive, chain) = chain_of(&[&a]);
        let originals = resolve(&pending(&mut archive, &chain, None, &[]), 1);

        let mut b = Source::new(day(2), &[0, 1], 6, 1).with_context(&context());
        b.add(originals.facts()[0].to_payload());
        if committed {
            b.add(SessionCarried {
                sources: originals.sources().to_vec(),
                failure_points: true,
                detail: None,
                carried: Vec::new(),
            });
        }
        let own = b
            .fail(0, -30, Condition::Alone, Attribution::Attributed)
            .end;

        let (mut archive, chain) = chain_of(&[&b, &a]);
        let got = resolve(&pending(&mut archive, &chain, None, &[]), 1);
        let mut want = vec![a.id_of(a_pass), b.id_of(own)];
        if !committed {
            want.insert(1, a.id_of(a_failure));
        }
        assert_eq!(ids(got.facts()), want, "committed {committed}");
    }
}

#[test]
fn reset_boundary_excludes_discarded_sources_and_copies() {
    let mut a = Source::new(day(1), &[0, 1], 6, 1).with_context(&context());
    a.pass(0, -30, Condition::Alone);
    a.fail(1, -30, Condition::Alone, Attribution::Attributed);
    let (mut archive, chain) = chain_of(&[&a]);
    let old = resolve(&pending(&mut archive, &chain, None, &[]), 1);
    assert_eq!(old.facts().len(), 2);

    let mut b = Source::new(day(2), &[0, 1], 6, 1).with_context(&context());
    carry_into(&mut b, &old, true);
    let own = b.pass(0, -30, Condition::Alone);

    let (mut archive, chain) = chain_of(&[&b]);
    archive.poison(a.id);
    let chain = [chain, vec![a.id]].concat();
    let got = resolve(&pending(&mut archive, &chain, Some(a.id), &[]), 1);
    assert_eq!(ids(got.facts()), [b.id_of(own)]);
    assert_eq!(
        archive.opened,
        [b.id],
        "a discarded archive is not even opened"
    );

    let behind = select(&[a.id], Some(a.id), &[], &mut archive).unwrap();
    assert!(behind.is_none());
    assert_eq!(
        archive.opened,
        [b.id],
        "a marked source behind the boundary is never read"
    );
}

#[test]
fn reset_boundary_filters_copied_seed_values() {
    let a = Source::new(day(1), &[0, 1], 5, 1).with_context(&context());
    let mut b = Source::new(day(2), &[0, 1], 6, 1).with_context(&context());
    b.add(SessionCarried {
        sources: vec![togi_journal::CarriedSource {
            session: a.id,
            schema: togi_journal::SCHEMA,
            ruleset: 5,
        }],
        failure_points: true,
        detail: None,
        carried: vec![
            seed(0, Some(limit(-30, a.id, super::seq(3))), None),
            seed(
                1,
                None,
                Some(point(-5, a.id, super::seq(4), Signal::ComputationError)),
            ),
        ],
    });
    let (mut archive, chain) = chain_of(&[&b]);
    archive.poison(a.id);
    let chain = [chain, vec![a.id]].concat();

    let behind = pending(&mut archive, &chain, Some(a.id), &[]);
    assert_eq!(behind.seeds(), []);

    let mut readable = Archive::of(&[&b, &a]);
    let kept = pending(&mut readable, &chain, Some(before()), &[]);
    assert_eq!(
        kept.seeds().len(),
        2,
        "a boundary older than the original keeps its values"
    );
}
