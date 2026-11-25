use std::collections::BTreeMap;

use serde_json::{Value, json};
use togi_journal::{FixStamp, SCHEMA, Seq, TrialId};
use togi_machine::{
    BiosContext, Condition, CoreId, CoreSet, PerCore, Regime, Signal, TargetOffset, WorkloadId,
};
use togi_tool_trialfacts::{BuildStamp, Class, Decisive, Phase, Profile, TrialClass, TrialFact};

pub(crate) fn core(n: i64) -> CoreId {
    CoreId::new(n).unwrap()
}

pub(crate) fn set(cores: &[i64]) -> CoreSet {
    cores.iter().map(|&n| core(n)).collect()
}

pub(crate) fn offsets(values: &[i64]) -> Vec<TargetOffset> {
    values
        .iter()
        .map(|&value| TargetOffset::new(value).unwrap())
        .collect()
}

pub(crate) fn workload(regime: Regime, index: usize) -> WorkloadId {
    WorkloadId::new(regime, index).unwrap()
}

pub(crate) fn bios(version: &str) -> BiosContext {
    BiosContext {
        bios_version: version.into(),
        board: "fixture board".into(),
        cpu_model: "fixture cpu".into(),
        microcode: "0x1".into(),
        boost_limit_mhz: 5000,
    }
}

pub(crate) fn stamp_of(build: u32) -> BuildStamp {
    BuildStamp {
        version: "fixture".into(),
        rev: "abc".into(),
        ruleset: build,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: Some(1),
    }
}

/// A decisive R1 trial fact of core 0 under the `fixture` BIOS; callers override what they test.
pub(crate) fn fact(session: &str, seq: u64, profile: &[i64], outcome: Decisive) -> TrialFact {
    TrialFact {
        session: session.parse().unwrap(),
        build: stamp_of(1),
        context: Some(bios("fixture")),
        seq: Seq::new(seq).unwrap(),
        trial: Some(TrialId::from("1".to_owned())),
        class: Class::Trial(TrialClass {
            workload: workload(Regime::R1, 0),
            loaded: set(&[0]),
            duration_s: 90,
        }),
        condition: Condition::Alone,
        phase: Some(Phase::Search),
        record_only: false,
        profile: Profile::new(&offsets(profile)).unwrap(),
        outcome,
        duration_s: 90,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
    }
}

pub(crate) fn crash(core: Option<i64>) -> Decisive {
    Decisive::Failure {
        signal: Signal::Crash,
        core: core.map(self::core),
        stalled: None,
    }
}

/// Builds fixture input, not a facts exporter. Keep callers independent of the reader's wire type.
pub(crate) fn row(fact: &TrialFact) -> Value {
    let (kind, workload, cores, duration) = match fact.class {
        Class::Trial(class) => (
            "trial",
            Some(class.workload),
            class.loaded,
            class.duration_s,
        ),
        Class::Idle { loaded } => ("idle", None, loaded, 0),
    };
    let (outcome, signal, core, stalled_core) = match fact.outcome {
        Decisive::Pass => ("pass", None, None, None),
        Decisive::Failure {
            signal,
            core,
            stalled,
        } => ("failure", Some(signal), core, stalled),
    };
    json!({
        "session": fact.session,
        "build": fact.build,
        "ruleset": fact.build.ruleset,
        "context": fact.context,
        "seq": fact.seq,
        "trial": fact.trial,
        "kind": kind,
        "class": {
            "regime": fact.class.regime(), "workload": workload, "cores": cores, "duration_s": duration
        },
        "condition": fact.condition,
        "phase": fact.phase,
        "record_only": fact.record_only,
        "profile": fact.profile.as_slice(),
        "outcome": outcome,
        "signal": signal,
        "duration_s": fact.duration_s,
        "stalled_core": stalled_core,
        "core": core,
        "voltage_requests_v": fact.voltage_requests_v,
        "top_requesters": fact.top_requesters,
        "ccd_mhz": fact.ccd_mhz,
    })
}

pub(crate) fn gz(text: &str) -> Vec<u8> {
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(text.as_bytes()).unwrap();
    encoder.finish().unwrap()
}
