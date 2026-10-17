mod activity;
mod carried;
mod decisive;
mod idle;
mod resets;
mod stream;

use std::collections::BTreeMap;
use std::convert::Infallible;

use jiff::Timestamp;
use togi_journal::{
    Attribution, Build, ConfigLoaded, DefectId, Event, Failure, FixStamp, Outcome, Payload, SCHEMA,
    Seq, SessionId, SessionStart, Stamp, TrialEnd, TrialId, TrialIntent,
};
use togi_machine::{
    BootId, CcdId, Condition, CoreId, CoreInfo, CoreSet, CpuId, PerCore, Regime, Signal,
    TargetOffset, WorkloadId,
};

use crate::{FactsError, SessionFacts, extract};

pub(super) fn core(n: i64) -> CoreId {
    CoreId::new(n).unwrap()
}

pub(super) fn set(cores: &[i64]) -> CoreSet {
    cores.iter().map(|&n| core(n)).collect()
}

pub(super) fn profile(pairs: &[(i64, i64)]) -> PerCore<TargetOffset> {
    pairs
        .iter()
        .map(|&(n, v)| (core(n), TargetOffset::new(v).unwrap()))
        .collect()
}

pub(super) fn offset(value: i64) -> TargetOffset {
    TargetOffset::new(value).unwrap()
}

pub(super) fn workload(regime: Regime) -> WorkloadId {
    WorkloadId::new(regime, 0).unwrap()
}

pub(super) fn trial(id: &str) -> TrialId {
    TrialId::from(id.to_owned())
}

pub(super) fn session(text: &str) -> SessionId {
    text.parse().unwrap()
}

pub(super) fn first() -> SessionId {
    session("20261001T000000Z")
}

pub(super) fn build(evidence: u32) -> Build {
    Build {
        version: "0.1.0".into(),
        rev: None,
        ruleset: 1,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence,
    }
}

/// A `config.loaded` build: the durable stamps of `base`, a newer version, rev and fixes.
pub(super) fn refreshed(base: &Build, version: &str) -> Build {
    Build {
        version: version.into(),
        rev: Some("rev".into()),
        fixes: FixStamp::through(DefectId::new(base.fixes.get() + 1).unwrap()),
        ..base.clone()
    }
}

pub(super) fn config_loaded(build: Build) -> ConfigLoaded {
    ConfigLoaded {
        build,
        config: togi_config::Config::default().snapshot(),
        path: "config.json".into(),
        file: false,
        kernel_cursor: None,
        kernel_error: None,
    }
}

pub(super) fn config_with_backends(mprime: &str, ycruncher: &str) -> togi_journal::ConfigSnapshot {
    togi_config::Config::parse(&format!(
        r#"{{"backends":{{"mprime":"{mprime}","ycruncher":"{ycruncher}"}}}}"#
    ))
    .unwrap()
    .snapshot()
}

pub(super) fn start(id: SessionId, cores: &[i64], evidence: u32) -> SessionStart {
    SessionStart {
        build: build(evidence),
        session: id,
        cores: cores
            .iter()
            .map(|&n| CoreInfo {
                core: core(n),
                ccd: CcdId(0),
                cpus: vec![CpuId(u16::try_from(n).unwrap())],
            })
            .collect(),
    }
}

pub(super) fn intent(id: &str, condition: Condition, regime: Regime) -> TrialIntent {
    TrialIntent {
        trial: trial(id),
        kernel_cursor: None,
        kernel_error: None,
        profile: PerCore::default(),
        core: None,
        cores: CoreSet::default(),
        offset: None,
        regime,
        workload: workload(regime),
        duration_s: 90,
        condition,
        activity: None,
        retry: false,
        cycle: None,
        step: None,
        hunt: None,
        group: None,
        round: None,
        rerun: false,
    }
}

pub(super) fn alone(id: &str, core_id: i64, value: i64, profile_cores: &[i64]) -> TrialIntent {
    TrialIntent {
        core: Some(core(core_id)),
        offset: Some(offset(value)),
        profile: profile_cores
            .iter()
            .map(|&n| (core(n), offset(if n == core_id { value } else { 0 })))
            .collect(),
        ..intent(id, Condition::Alone, Regime::R1)
    }
}

pub(super) fn end(id: &str, outcome: Outcome, duration_s: u32) -> TrialEnd {
    TrialEnd {
        trial: trial(id),
        kernel_cursor: None,
        kernel_error: None,
        outcome,
        signal: None,
        core: None,
        duration_s,
        tctl_max_c: None,
        voltage_request_median_v: None,
        voltage_request_min_v: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
        last_sample_s: None,
        last_sample_tctl_c: None,
        last_sample_min_mhz: None,
        last_sample_max_mhz: None,
        stalled_core: None,
        worker_stalled_ms: None,
        reason: None,
        interrupted: false,
        last_evidence: false,
        escaped: Vec::new(),
        backend_missing: false,
        containment_error: None,
    }
}

pub(super) fn pass(id: &str, duration_s: u32) -> TrialEnd {
    end(id, Outcome::Pass, duration_s)
}

pub(super) fn fail(id: &str, signal: Signal, duration_s: u32) -> TrialEnd {
    TrialEnd {
        signal: Some(signal),
        ..end(id, Outcome::Failure, duration_s)
    }
}

pub(super) fn idle(
    condition: Option<Condition>,
    profile: Option<PerCore<TargetOffset>>,
) -> Failure {
    Failure {
        signal: Signal::Crash,
        attribution: Attribution::Unattributed,
        core: None,
        offset: None,
        trial: None,
        regime: None,
        condition,
        profile,
        known_failure: None,
        reason: None,
        round: None,
    }
}

pub(super) struct Script {
    events: Vec<Event>,
    boot: BootId,
}

impl Script {
    pub(super) fn new(boot: &str) -> Self {
        Self {
            events: Vec::new(),
            boot: BootId::from(boot.to_owned()),
        }
    }

    pub(super) fn begin(cores: &[i64]) -> Self {
        let mut script = Self::new("a");
        script.add(start(first(), cores, 1));
        script
    }

    pub(super) fn boot(&mut self, boot: &str) {
        self.boot = BootId::from(boot.to_owned());
    }

    pub(super) fn add(&mut self, payload: impl Into<Payload>) -> Seq {
        self.add_caused(&[], payload)
    }

    pub(super) fn add_caused(&mut self, cause: &[Seq], payload: impl Into<Payload>) -> Seq {
        let seq = Seq::new(self.events.len() as u64 + 1).unwrap();
        let stamp = Stamp {
            time: Self::time(seq),
            mono: None,
        };
        self.events.push(Event::new(
            seq,
            stamp,
            self.boot.clone(),
            cause.to_vec(),
            payload.into(),
        ));
        seq
    }

    pub(super) fn time(seq: Seq) -> Timestamp {
        Timestamp::from_second(1_700_000_000 + i64::try_from(seq.get()).unwrap()).unwrap()
    }

    pub(super) fn extract(&self) -> SessionFacts {
        self.try_extract(first()).unwrap()
    }

    pub(super) fn try_extract(
        &self,
        id: SessionId,
    ) -> Result<SessionFacts, FactsError<Infallible>> {
        extract(id, self.events.iter().cloned().map(Ok))
    }
}
