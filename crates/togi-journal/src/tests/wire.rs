use std::collections::BTreeSet;
use std::time::Duration;

use jiff::Timestamp;

use super::fixtures::{boot, samples, seq};
use crate::*;

fn stamp(mono: Option<Duration>) -> Stamp {
    Stamp {
        time: "2026-10-02T01:14:07.120Z".parse::<Timestamp>().unwrap(),
        mono,
    }
}

#[test]
fn samples_cover_every_kind_once() {
    let kinds: BTreeSet<_> = samples().iter().map(Payload::kind).collect();
    assert_eq!(kinds.len(), samples().len());
    assert_eq!(kinds, EventKind::ALL.iter().copied().collect());
    assert_eq!(EventKind::ALL.len(), 50);
}

#[test]
fn every_kind_round_trips_through_the_wire() {
    for (index, payload) in samples().into_iter().enumerate() {
        let kind = payload.kind();
        let seq_no = u64::try_from(index).unwrap() + 2;
        let event = Event::new(
            seq(seq_no),
            stamp(Some(Duration::from_millis(5))),
            boot(),
            vec![seq(1)],
            payload,
        );
        assert!(!event.msg.is_empty(), "{kind} has no message");
        let line = event.encode().unwrap();
        assert!(!line.contains('\n'));
        let decoded =
            Event::decode(&line).unwrap_or_else(|error| panic!("{kind}: {error}\n{line}"));
        assert_eq!(decoded, event, "{kind}");
        assert_eq!(decoded.encode().unwrap(), line, "{kind}");
    }
}

#[test]
fn kind_names_round_trip() {
    for kind in EventKind::ALL {
        assert_eq!(kind.as_str().parse::<EventKind>().unwrap(), *kind);
    }
    assert!("trial.nope".parse::<EventKind>().is_err());
}

proptest::proptest! {
    #[test]
    fn profiles_causes_and_mono_round_trip(
        offsets in proptest::collection::vec(-50_i64..=0, 0..16),
        causes in proptest::collection::btree_set(1_u64..1000, 0..5),
        mono in proptest::option::of(0_u64..1_000_000),
    ) {
        let payload = Payload::from(ProfileApplied {
            offsets: super::fixtures::profile(&offsets),
            condition: togi_machine::Condition::Together,
        });
        let mut event = Event::new(
            seq(1000),
            stamp(mono.map(Duration::from_millis)),
            boot(),
            causes.into_iter().map(seq).collect(),
            payload,
        );
        event.msg = "x".into();
        let decoded = Event::decode(&event.encode().unwrap()).unwrap();
        proptest::prop_assert_eq!(decoded, event);
    }
}
