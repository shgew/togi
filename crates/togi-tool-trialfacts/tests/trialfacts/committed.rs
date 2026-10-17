use std::collections::BTreeSet;
use std::io::Read;

use flate2::read::MultiGzDecoder;
use togi_facts::Decisive;
use togi_journal::Seq;
use togi_machine::{BiosContext, Condition, Regime};
use togi_sim::replay::Replay;
use togi_tool_trialfacts::{Class, Phase, ProfileClassKey, decode, encode, replay_facts};

use crate::builders::bios;

const COMMITTED: &[u8] = include_bytes!("../../../../tools/bench/facts/target.jsonl.gz");

#[test]
fn the_committed_extract_reads_unchanged() {
    let facts = decode(COMMITTED).unwrap();
    assert_eq!(facts.len(), 3889);

    let context = facts[0].context.clone().unwrap();
    let target = BiosContext {
        bios_version: "4.43".into(),
        board: "ASRock X870E Taichi".into(),
        cpu_model: "AMD Ryzen 9 9950X3D2 16-Core Processor".into(),
        microcode: "0xb404038".into(),
        boost_limit_mhz: 5650,
    };
    assert_eq!(context, target);
    assert_ne!(context, bios("fixture"));
    assert!(facts.iter().all(|f| f.context.as_ref() == Some(&target)));
    assert!(facts.iter().all(|f| matches!(f.class, Class::Trial(_))));
    assert!(facts.iter().all(|f| f.profile.as_slice().len() == 16));
    assert!(facts.iter().all(|f| f.trial.is_some()));

    let failures = facts
        .iter()
        .filter(|f| matches!(f.outcome, Decisive::Failure { .. }))
        .count();
    assert_eq!((failures, facts.len() - failures), (242, 3647));
    assert_eq!(facts.iter().filter(|f| f.record_only).count(), 16);
    assert_eq!(
        facts
            .iter()
            .filter(|f| f.phase == Some(Phase::Confirmation))
            .count(),
        530
    );
    for (condition, count) in [
        (Condition::Alone, 1340),
        (Condition::Parked, 1215),
        (Condition::Together, 1334),
    ] {
        assert_eq!(
            facts.iter().filter(|f| f.condition == condition).count(),
            count,
            "{condition}"
        );
    }
    for (regime, count) in [
        (Regime::R1, 904),
        (Regime::R2, 977),
        (Regime::R3, 133),
        (Regime::R4, 121),
        (Regime::R5, 73),
        (Regime::R6, 6),
        (Regime::R7, 1675),
    ] {
        assert_eq!(
            facts.iter().filter(|f| f.class.regime() == regime).count(),
            count,
            "{regime}"
        );
    }
    let mut rulesets: Vec<_> = facts
        .iter()
        .fold(std::collections::BTreeMap::new(), |mut counts, f| {
            *counts.entry(f.build.ruleset).or_insert(0_usize) += 1;
            counts
        })
        .into_iter()
        .collect();
    rulesets.sort_unstable();
    assert_eq!(
        rulesets,
        [
            (1, 803),
            (2, 479),
            (3, 403),
            (4, 894),
            (6, 317),
            (7, 264),
            (8, 137),
            (9, 149),
            (11, 443)
        ]
    );
    assert!(facts.iter().all(|f| f.seq >= Seq::new(1).unwrap()));
    let sessions: BTreeSet<_> = facts.iter().map(|f| f.session).collect();
    assert_eq!(sessions.len(), 9);
    let classes: BTreeSet<_> = facts
        .iter()
        .map(|f| format!("{:?}", ProfileClassKey::from(f)))
        .collect();
    assert!(classes.len() > 100);
}

#[test]
fn every_committed_record_writes_back_to_the_same_json() {
    let mut text = String::new();
    MultiGzDecoder::new(COMMITTED)
        .read_to_string(&mut text)
        .unwrap();
    let facts = decode(COMMITTED).unwrap();
    assert_eq!(text.lines().count(), facts.len());
    for (line, fact) in text.lines().zip(&facts) {
        let original: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(serde_json::to_value(fact).unwrap(), original, "{line}");
    }
}

#[test]
fn the_committed_extract_survives_a_rewrite() {
    let facts = decode(COMMITTED).unwrap();
    let mut bytes = Vec::new();
    encode(&mut bytes, &facts).unwrap();
    assert_eq!(decode(bytes.as_slice()).unwrap(), facts);
}

#[test]
fn the_committed_extract_builds_a_replay_oracle() {
    let facts = decode(COMMITTED).unwrap();
    let replayed = replay_facts(&facts);
    assert_eq!(replayed.len(), facts.len());
    let replay = Replay::new(replayed).unwrap();
    assert_eq!(replay.len(), 3889);
}
