use std::collections::BTreeSet;
use std::time::Duration;

use jiff::Timestamp;
use proptest::prelude::Strategy;

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
    assert_eq!(EventKind::ALL.len(), 73);
}

#[test]
fn every_kind_round_trips_through_the_wire() {
    for (index, payload) in samples().into_iter().enumerate() {
        let kind = payload.kind();
        let message = payload.message();
        let seq_no = u64::try_from(index).unwrap() + 2;
        let event = Event::new(
            seq(seq_no),
            stamp(Some(Duration::from_millis(5))),
            boot(),
            vec![seq(1)],
            payload,
        );
        assert!(!message.is_empty(), "{kind} has no message");
        assert_eq!(event.msg, message, "{kind} stores its generated message");
        let line = event.encode().unwrap();
        assert!(!line.contains('\n'), "{kind}");
        let decoded =
            Event::decode(&line).unwrap_or_else(|error| panic!("{kind}: {error}\n{line}"));
        assert_eq!(decoded, event, "{kind}");
        assert_eq!(decoded.encode().unwrap(), line, "{kind}");
    }
}

#[test_case::test_case(togi_machine::Regime::R8; "neighbours")]
#[test_case::test_case(togi_machine::Regime::R9; "all threads")]
fn trial_roles_round_trip_with_logical_cpu_workload_and_phase_identity(
    regime: togi_machine::Regime,
) {
    use togi_machine::{CcdId, CoreId, CoreInfo, CpuId, Regime, TrialRole, pick_workload};

    let topology: Vec<_> = (0_u16..4)
        .map(|id| CoreInfo {
            core: CoreId::new(i64::from(id)).unwrap(),
            ccd: CcdId::new(0).unwrap(),
            cpus: vec![CpuId::from(id), CpuId::from(id + 4)],
        })
        .collect();
    let roles = match regime {
        Regime::R8 => TrialRole::neighbours(
            pick_workload(Regime::R8, 0).id(),
            topology[2].core,
            pick_workload(Regime::R2, 0).id(),
            42,
            &topology,
        )
        .unwrap(),
        Regime::R9 => TrialRole::all_threads(
            pick_workload(Regime::R9, 0).id(),
            topology[2].cpus[1],
            &topology,
        )
        .unwrap(),
        Regime::R1
        | Regime::R2
        | Regime::R3
        | Regime::R4
        | Regime::R5
        | Regime::R6
        | Regime::R7 => unreachable!("parametric inputs are the mixed-role regimes"),
    };
    let Payload::TrialStart(mut start) = super::fixtures::sample(EventKind::TrialStart) else {
        unreachable!();
    };
    start.roles = roles.clone();
    let event = Event::new(seq(2), stamp(None), boot(), vec![seq(1)], start.into());
    let wire = event.encode().unwrap();
    let decoded = Event::decode(&wire).unwrap();
    assert_eq!(decoded, event);
    let Payload::TrialStart(decoded) = decoded.payload else {
        unreachable!();
    };
    assert_eq!(decoded.roles, roles);
    let legacy = super::fixtures::sample(EventKind::TrialStart);
    let legacy = Event::new(seq(2), stamp(None), boot(), vec![seq(1)], legacy);
    let Payload::TrialStart(legacy) = Event::decode(&legacy.encode().unwrap()).unwrap().payload
    else {
        unreachable!();
    };
    assert_eq!(legacy.roles, []);
}

/// The line of every sample payload, pinned: the kind names and the payload fields and numbers on the wire.
#[test]
fn every_kind_matches_its_golden() {
    let lines: Vec<String> = samples()
        .into_iter()
        .map(|payload| {
            Event::new(seq(2), stamp(None), boot(), vec![seq(1)], payload)
                .encode()
                .unwrap()
        })
        .collect();
    let golden = include_str!("snapshots/every_kind.jsonl");
    assert_eq!(lines.len(), golden.lines().count());
    for (line, pinned) in lines.iter().zip(golden.lines()) {
        assert_eq!(line, pinned);
    }
}

#[test]
fn kind_names_round_trip() {
    for kind in EventKind::ALL {
        assert_eq!(kind.as_str().parse::<EventKind>().unwrap(), *kind, "{kind}");
        let wire = serde_json::to_value(kind).unwrap();
        assert_eq!(wire, serde_json::json!(kind.as_str()), "{kind}");
        assert_eq!(
            serde_json::from_value::<EventKind>(wire).unwrap(),
            *kind,
            "{kind}"
        );
    }
    assert!("trial.nope".parse::<EventKind>().is_err());
    assert!(serde_json::from_str::<EventKind>(r#""trial.nope""#).is_err());
    assert!(serde_json::from_str::<EventKind>("0").is_err());
}

/// Any known event: a sample payload of any kind with arbitrary seq, mono time and causes.
pub(super) fn arbitrary_event() -> impl Strategy<Value = Event> {
    (
        0..EventKind::ALL.len(),
        1001_u64..2000,
        proptest::option::of(0_u64..1_000_000),
        proptest::collection::btree_set(1_u64..1000, 0..5),
    )
        .prop_map(|(index, seq_no, mono, causes)| {
            Event::new(
                seq(seq_no),
                stamp(mono.map(Duration::from_millis)),
                boot(),
                causes.into_iter().map(seq).collect(),
                samples().swap_remove(index),
            )
        })
}

proptest::proptest! {
    #![proptest_config(crate::testing::property_config())]
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
