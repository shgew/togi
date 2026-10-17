use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use togi_facts::{Decisive, TrialClass};
use togi_journal::{FixStamp, SCHEMA, Seq};
use togi_machine::{
    BiosContext, CcdId, Condition, CoreId, CoreSet, PerCore, Regime, Signal, TargetOffset,
    WorkloadId,
};
use togi_tool_trialfacts::{BuildStamp, Class, Phase, Profile, TrialFact, write_unsynced};

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
            fixes: FixStamp::none(),
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

/// Three sessions of a sixteen-core machine: solo R1 trials and R7 trials of each CCD at several depths, with
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
                    let mut offsets = [0_i64; 16];
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
                    let mut offsets = [0_i64; 16];
                    offsets[usize::try_from(ccd * 8).unwrap()] = -depth;
                    offsets[usize::try_from(ccd * 8 + 1).unwrap()] = -depth;
                    let failing = (s * 19 + ccd * 7 + depth + rep * 11) % 20 < depth / 5;
                    seq += 1;
                    facts.push(fact(
                        session,
                        seq,
                        &offsets,
                        Regime::R7,
                        &[ccd * 8, ccd * 8 + 1],
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
    #[expect(
        clippy::disallowed_methods,
        reason = "a disposable unsynced fixture feeds the real CLI subprocess"
    )]
    write_unsynced(&path, facts).unwrap();
    path
}

fn named_failure(core: i64) -> Decisive {
    Decisive::Failure {
        signal: Signal::ComputationError,
        core: Some(CoreId::new(core).unwrap()),
        stalled: None,
    }
}

/// One session of a sixteen-core machine: R7 trials of each CCD at several depths, with their voltage
/// requests and CCD clocks and failures that name a loaded core, and solo R1 trials of four cores.
pub(crate) fn sixteen_core_evidence() -> Vec<TrialFact> {
    let session = "20260101T000000Z";
    let mut facts = Vec::new();
    let mut seq = 0;
    for ccd in 0..2_i64 {
        let loaded: Vec<i64> = (ccd * 8..ccd * 8 + 8).collect();
        for rep in 0..10_i64 {
            let mut offsets = [0_i64; 16];
            let mut total = 0;
            for (slot, &core) in loaded.iter().enumerate() {
                let slot = i64::try_from(slot).unwrap();
                let depth = 10 + (rep * 7 + slot * 5 + ccd * 3) % 31;
                offsets[usize::try_from(core).unwrap()] = -depth;
                total += depth;
            }
            let mean_depth = total / 8;
            let failing = (ccd * 7 + mean_depth + rep * 11) % 20 < mean_depth / 5;
            seq += 1;
            let mut record = fact(session, seq, &offsets, Regime::R7, &loaded, 120, failing);
            if failing {
                record.outcome = named_failure(loaded[usize::try_from(rep * 3 % 8).unwrap()]);
            }
            let mhz = 5200 + u32::try_from(rep % 4).unwrap() * 20;
            record
                .ccd_mhz
                .insert(CcdId(u8::try_from(ccd).unwrap()), mhz);
            for &core in &loaded {
                let depth = -offsets[usize::try_from(core).unwrap()];
                let request = 1.22 + f64::from(u8::try_from(core % 8).unwrap()) * 0.005
                    - 0.0038 * f64::from(u8::try_from(depth).unwrap())
                    + 0.03 * (f64::from(mhz) - 5240.0) / 100.0;
                record
                    .voltage_requests_v
                    .set(CoreId::new(core).unwrap(), request);
            }
            facts.push(record);
        }
    }
    for core in [0_i64, 5, 8, 13] {
        for depth in [10_i64, 20, 30, 40] {
            for rep in 0..5_i64 {
                let mut offsets = [0_i64; 16];
                offsets[usize::try_from(core).unwrap()] = -depth;
                let failing = (core * 17 + depth * 3 + rep * 13) % 10 < depth / 10;
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
    facts
}
