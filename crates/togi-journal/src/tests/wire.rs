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
        let decoded = Event::decode(&line).unwrap_or_else(|error| panic!("{kind}: {error}\n{line}"));
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
