use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use togi_facts::{Decisive, TrialClass};
use togi_journal::{SCHEMA, Seq};
use togi_machine::{
    BiosContext, Condition, CoreId, CoreSet, PerCore, Regime, Signal, TargetOffset, WorkloadId,
};
use togi_tool_trialfacts::{BuildStamp, Class, Phase, Profile, TrialFact, write};

pub(crate) fn fact(
    session: &str,
    seq: u64,
    offsets: &[i64],
    regime: Regime,
    loaded: &[i64],
    duration_s: u32,
    failing: bool,
) -> TrialFact {
    let offsets: Vec<_> = offsets
        .iter()
        .map(|&offset| TargetOffset::new(offset).unwrap())
        .collect();
    TrialFact {
        session: session.parse().unwrap(),
        build: BuildStamp {
            version: "fixture".into(),
            rev: "abc".into(),
            ruleset: 1,
            schema: SCHEMA,
            fixes: 0,
            evidence: Some(1),
        },
        context: Some(BiosContext {
            bios_version: "A".into(),
            board: "fixture".into(),
            cpu_model: "Zen 5 fixture".into(),
            microcode: "0x1".into(),
            boost_limit_mhz: 5600,
        }),
        seq: Seq::new(seq).unwrap(),
        trial: None,
        class: Class::Trial(TrialClass {
            workload: WorkloadId::new(regime, 0).unwrap(),
            loaded: loaded.iter().map(|&c| CoreId::new(c).unwrap()).collect(),
            duration_s,
        }),
        condition: Condition::Alone,
        phase: Some(Phase::Search),
        record_only: false,
        profile: Profile::new(&offsets).unwrap(),
        outcome: if failing {
            Decisive::Failure {
                signal: Signal::Crash,
                core: None,
                stalled: None,
            }
        } else {
            Decisive::Pass
        },
        duration_s,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
    }
}

/// Three sessions of a four-core machine: solo R1 trials and R7 trials of each CCD at several depths, with
/// failures that grow with depth by a fixed pattern.
pub(crate) fn evidence() -> Vec<TrialFact> {
    let mut facts = Vec::new();
    let mut seq = 0;
    for (s, session) in ["20260101T000000Z", "20260102T000000Z", "20260103T000000Z"]
        .into_iter()
        .enumerate()
    {
        let s = i64::try_from(s).unwrap();
        for core in 0..4_i64 {
            for depth in [10, 20, 30, 40] {
                for rep in 0..5 {
                    let mut offsets = [0_i64; 4];
                    offsets[usize::try_from(core).unwrap()] = -depth;
                    let failing = (s * 31 + core * 17 + depth * 3 + rep * 13) % 10 < depth / 10;
                    seq += 1;
                    facts.push(fact(
                        session,
                        seq,
                        &offsets,
                        Regime::R1,
                        &[core],
                        60,
                        failing,
                    ));
                }
            }
        }
        for ccd in 0..2_i64 {
            for depth in [15, 20, 25, 30, 35, 40] {
                for rep in 0..6 {
                    let mut offsets = [0_i64; 4];
                    offsets[usize::try_from(ccd * 2).unwrap()] = -depth;
                    offsets[usize::try_from(ccd * 2 + 1).unwrap()] = -depth;
                    let failing = (s * 19 + ccd * 7 + depth + rep * 11) % 20 < depth / 5;
                    seq += 1;
                    facts.push(fact(
                        session,
                        seq,
                        &offsets,
                        Regime::R7,
                        &[ccd * 2, ccd * 2 + 1],
                        120,
                        failing,
                    ));
                }
            }
        }
    }
    facts
}

pub(crate) fn write_extract(dir: &Path, facts: &[TrialFact]) -> PathBuf {
    let path = dir.join("facts.jsonl.gz");
    write(&path, facts).unwrap();
    path
}
