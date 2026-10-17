//! Shared fixtures of the unit tests: facts, machines and synthetic evidence.

use std::collections::BTreeMap;

use togi_facts::{Decisive, TrialClass};
use togi_journal::{SCHEMA, Seq};
use togi_machine::{
    BiosContext, Condition, CoreId, CoreSet, PerCore, Regime, Signal, TargetOffset, WorkloadId,
};
use togi_sim::MachineDefinition;
use togi_tool_trialfacts::{BuildStamp, Class, Phase, Profile, TrialFact};

mod anchor;
mod artifact;
mod evidence;
mod forward;
mod generate;
#[path = "../../tests/fit/output.rs"]
mod output;
mod search;
mod signals;
mod voltage;

pub(crate) const SESSION: &str = "20260101T000000Z";

pub(crate) fn cores(ids: &[i64]) -> CoreSet {
    ids.iter().map(|&id| CoreId::new(id).unwrap()).collect()
}

pub(crate) fn context() -> BiosContext {
    BiosContext {
        bios_version: "A".into(),
        board: "fixture".into(),
        cpu_model: "Zen 5 fixture".into(),
        microcode: "0x1".into(),
        boost_limit_mhz: 5600,
    }
}

pub(crate) fn crash() -> Decisive {
    failure(Signal::Crash)
}

pub(crate) fn failure(signal: Signal) -> Decisive {
    Decisive::Failure {
        signal,
        core: None,
        stalled: None,
    }
}

pub(crate) fn profile(offsets: &[i64]) -> Profile {
    let offsets: Vec<_> = offsets
        .iter()
        .map(|&offset| TargetOffset::new(offset).unwrap())
        .collect();
    Profile::new(&offsets).unwrap()
}

/// A trial of the first workload of `regime`, in the default session and BIOS context.
pub(crate) fn fact(
    offsets: &[i64],
    regime: Regime,
    loaded: &[i64],
    duration_s: u32,
    outcome: Decisive,
) -> TrialFact {
    fact_at(SESSION, 1, offsets, regime, 0, loaded, duration_s, outcome)
}

/// `offsets` extended with stock offsets to the 16 cores the shared-voltage fit requires.
fn padded(offsets: &[i64]) -> Vec<i64> {
    let mut padded = offsets.to_vec();
    if padded.len() < 16 {
        padded.resize(16, 0);
    }
    padded
}

#[expect(
    clippy::too_many_arguments,
    reason = "a fixture names every field it varies"
)]
pub(crate) fn fact_at(
    session: &str,
    seq: u64,
    offsets: &[i64],
    regime: Regime,
    workload: usize,
    loaded: &[i64],
    duration_s: u32,
    outcome: Decisive,
) -> TrialFact {
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
        context: Some(context()),
        seq: Seq::new(seq).unwrap(),
        trial: None,
        class: Class::Trial(TrialClass {
            workload: WorkloadId::new(regime, workload).unwrap(),
            loaded: cores(loaded),
            duration_s,
        }),
        condition: Condition::Alone,
        phase: Some(Phase::Search),
        record_only: false,
        profile: profile(&padded(offsets)),
        outcome,
        duration_s,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
    }
}

/// `n` repetitions of a trial, the first `failures` of them failing.
pub(crate) fn repeated(template: &TrialFact, n: usize, failures: usize) -> Vec<TrialFact> {
    (0..n)
        .map(|i| {
            let mut fact = template.clone();
            fact.outcome = if i < failures {
                crash()
            } else {
                Decisive::Pass
            };
            fact
        })
        .collect()
}

/// A machine of `n` cores with every limit at the deepest offset and the model's defaults.
pub(crate) fn blank_machine(n: usize) -> MachineDefinition {
    MachineDefinition {
        cores: i64::try_from(n).unwrap(),
        core: (0..n).map(crate::evidence::core_def).collect(),
        ..MachineDefinition::default()
    }
}
