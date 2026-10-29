use serde_json::json;
use test_case::test_case;
use togi_machine::CcdId;

use super::fixtures::{at, bios, boot, core, profile, seq, session, set, target};
use crate::*;

fn unit() -> UnitId {
    UnitId::Ccd(CcdId::new(0).unwrap())
}

fn mapping() -> UnitMapping {
    UnitMapping {
        phase: UnitPhase::Coarse,
        units: vec![UnitMembership {
            unit: unit(),
            cores: set(&[0, 1]),
            parent: None,
        }],
    }
}

fn vector() -> UnitVector {
    UnitVector {
        cores: profile(&[-5, -5]),
        mapping: mapping(),
    }
}

fn event() -> DayEventId {
    DayEventId::Boot { boot: boot() }
}

fn campaign() -> CampaignId {
    CampaignId(Provenance {
        session: session(),
        seq: seq(1),
    })
}

fn hold() -> ClimbHold {
    ClimbHold {
        application: ApplicationId(2),
        credited: vec![seq(3)],
        eligible_s: 180_000,
    }
}

pub(super) fn samples() -> Vec<Payload> {
    vec![
        mapping().into(),
        UnitEdge {
            unit: unit(),
            firmware: bios(),
            epoch: 1,
            old: Edge::Unknown {},
            new: Edge::Observed {
                offset: target(-30),
                witness: seq(4),
            },
            ceiling: Some(target(-25)),
            margin: 5,
        }
        .into(),
        ClimbDecision {
            decision: ClimbAction::Promote {
                unit: unit(),
                from: target(0),
                to: target(-5),
                step: 5,
            },
            cold_boot: boot(),
            before: UnitVector {
                cores: profile(&[0, 0]),
                mapping: mapping(),
            },
            proposed: vector(),
            hold: hold(),
            certificate: Some(seq(5)),
            ceilings: vec![seq(4)],
            events: vec![],
        }
        .into(),
        UnitStop {
            unit: unit(),
            reason: UnitStopReason::FirstBackstep {
                event: event(),
                application: ApplicationId(2),
            },
            profile: vector(),
            last_clean: target(-5),
            witnesses: vec![seq(6)],
        }
        .into(),
        CampaignStart {
            campaign: campaign(),
            allowance: 2,
            origin: CampaignOrigin::Fresh {},
            history: CampaignHistory::Complete {},
        }
        .into(),
        CampaignCharge {
            campaign: campaign(),
            event: event(),
            aliases: vec![DayEventId::Record {
                session: session(),
                seq: seq(7),
            }],
            profile: vector(),
            phase: CampaignEventPhase::Learning,
            unit_phase: UnitPhase::Coarse,
            provisional: true,
            consumed: 1,
            remaining: 1,
            exhausted: false,
        }
        .into(),
        CampaignReclassified {
            campaign: campaign(),
            event: event(),
            from: Classification::CpuSuspect {
                certainty: Certainty::Provisional,
            },
            to: Classification::NonCpu {},
        }
        .into(),
        FirmwareRestoration {
            saved: vector(),
            state: FirmwareRestorationState::Qualified {
                certificate: seq(5),
            },
        }
        .into(),
        SuccessorImported {
            sources: vec![CarriedSource {
                session: session(),
                schema: 1,
                ruleset: 3,
            }],
            counts: ImportCounts {
                selected: 2,
                imported: 2,
                duplicates: 0,
                unknown: 0,
                excluded: 0,
            },
            vetoes: vec![Provenance {
                session: session(),
                seq: seq(6),
            }],
            night_observations: vec![Provenance {
                session: session(),
                seq: seq(4),
            }],
            campaign: CampaignHistory::Unreconstructible {
                reason: "application history missing".into(),
            },
        }
        .into(),
    ]
}

#[test]
fn every_ruleset14_kind_round_trips_strictly() {
    let header = at(
        1,
        "boot-a",
        0,
        &[],
        super::fixtures::sample(EventKind::SessionStart),
    );
    let header = header.encode().unwrap();
    for payload in samples() {
        let kind = payload.kind();
        let expected = at(2, "boot-a", 0, &[1], payload);
        let encoded = expected.encode().unwrap();
        assert_eq!(Event::decode(&encoded).unwrap(), expected, "{kind}");
        let journal = format!("{header}\n{encoded}\n");
        let mut reader = EventReader::lenient(journal.as_bytes());
        reader.next().unwrap().unwrap();
        assert_eq!(reader.next().unwrap().unwrap(), expected, "{kind}");
        assert!(reader.next().is_none(), "{kind}");
        let mut unknown: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        unknown["data"]["extra"] = json!(1);
        let unknown = unknown.to_string();
        assert!(
            matches!(
                Event::decode(&unknown),
                Err(DecodeError::UnknownField { .. })
            ),
            "{kind}"
        );
        let journal = format!("{header}\n{unknown}\n");
        let mut reader = EventReader::lenient(journal.as_bytes());
        reader.next().unwrap().unwrap();
        assert_eq!(reader.next().unwrap().unwrap(), expected, "{kind}");
    }
}

#[test]
fn historical_ccd_edge_and_cap_keep_their_meaning() {
    let mut found = Vec::new();
    for line in include_str!("snapshots/every_kind.jsonl").lines() {
        let envelope: serde_json::Value = serde_json::from_str(line).unwrap();
        if envelope["kind"] == "ccd.edge" || envelope["kind"] == "ccd.cap" {
            let decoded = Event::decode(line).unwrap();
            assert_eq!(decoded.encode().unwrap(), line);
            assert_eq!(decoded.payload, super::fixtures::sample(decoded.kind()));
            if let Payload::CcdEdge(edge) = &decoded.payload {
                assert_eq!(edge.ccd, CcdId::new(0).unwrap());
                assert_eq!(
                    edge.edge,
                    Edge::Observed {
                        offset: target(-25),
                        witness: seq(40)
                    }
                );
            }
            if let Payload::CcdCap(cap) = &decoded.payload {
                assert_eq!(cap.old, target(-10));
                assert_eq!(cap.new, target(-5));
                assert_eq!(
                    cap.edge,
                    Edge::Censored {
                        limit: target(-50),
                        witnesses: vec![seq(30), seq(31)]
                    }
                );
            }
            found.push(decoded.kind());
        }
    }
    assert_eq!(found, [EventKind::CcdEdge, EventKind::CcdCap]);
    assert_eq!(SCHEMA, 1);
}

#[test_case(ClimbAction::Hold { blocked_by: vec![ClimbBlocker::CertificateFailed { profile: vector(), witnesses: vec![seq(7)] }] }; "hold with nested failed vector")]
#[test_case(ClimbAction::Retreat { units: vec![unit()], cause: ClimbRetreatCause::Event { event: event() }, wanted: Some(vector()) }; "retreat with wanted vector")]
#[test_case(ClimbAction::Restore { restoration: RestorationKind::Firmware, units: vec![unit()] }; "firmware restoration")]
#[test_case(ClimbAction::Restore { restoration: RestorationKind::Reclassified, units: vec![unit()] }; "reclassification restoration")]
#[test_case(ClimbAction::FirmwareZero {}; "firmware zero")]
fn climb_actions_preserve_nested_profiles(action: ClimbAction) {
    let payload = ClimbDecision {
        decision: action,
        cold_boot: boot(),
        before: vector(),
        proposed: vector(),
        hold: hold(),
        certificate: None,
        ceilings: vec![],
        events: vec![],
    };
    let expected = at(2, "boot-a", 0, &[1], payload.into());
    assert_eq!(
        Event::decode(&expected.encode().unwrap()).unwrap(),
        expected
    );
}

#[test]
fn core_profiles_preserve_nonuniform_offsets_and_coarse_ancestry() {
    let mapping = UnitMapping {
        phase: UnitPhase::Core,
        units: vec![
            UnitMembership {
                unit: UnitId::Core(core(0)),
                cores: set(&[0]),
                parent: Some(unit()),
            },
            UnitMembership {
                unit: UnitId::Core(core(1)),
                cores: set(&[1]),
                parent: Some(unit()),
            },
        ],
    };
    let saved = UnitVector {
        cores: profile(&[-6, -5]),
        mapping,
    };
    let expected = at(
        2,
        "boot-a",
        0,
        &[1],
        FirmwareRestoration {
            saved: saved.clone(),
            state: FirmwareRestorationState::Held { hold: hold() },
        }
        .into(),
    );
    let encoded = expected.encode().unwrap();
    assert_eq!(Event::decode(&encoded).unwrap(), expected);
    let wire: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        wire["data"]["saved"],
        json!({"cores":{"0":-6,"1":-5},"mapping":{"phase":"core","units":[{"unit":{"core":0},"cores":[0],"parent":{"ccd":0}},{"unit":{"core":1},"cores":[1],"parent":{"ccd":0}}]}})
    );
    assert_eq!(saved.cores, profile(&[-6, -5]));
}

#[test_case("{}"; "missing identity")]
#[test_case(r#"{"ccd":0,"core":0}"#; "two identities")]
#[test_case(r#"{"ccd":null}"#; "null CCD")]
#[test_case(r#"{"core":null}"#; "null core")]
#[test_case(r#"{"ccd":0,"ccd":1}"#; "duplicate identity")]
#[test_case(r#"{"core":-1}"#; "invalid core")]
#[test_case("[0]"; "positional identity")]
fn unit_identity_refuses_ambiguous_or_malformed_objects(wire: &str) {
    assert!(serde_json::from_str::<UnitId>(wire).is_err());
}
