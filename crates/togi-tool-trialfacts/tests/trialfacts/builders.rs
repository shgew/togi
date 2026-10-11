use std::collections::BTreeMap;

use jiff::Timestamp;
use togi_facts::{Decisive, TrialClass};
use togi_journal::{
    Activity, ArchiveKind, Build, Journal, MemFs, Outcome, Payload, SCHEMA, Seq, SessionContext,
    SessionStart, Stamp, StateDir, TrialEnd, TrialId, TrialIntent,
};
use togi_machine::{
    BiosContext, BootId, CcdId, Condition, CoreId, CoreInfo, CoreSet, CpuId, PerCore, Regime,
    Signal, TargetOffset, WorkloadId,
};
use togi_tool_trialfacts::{BuildStamp, Class, Phase, Profile, TrialFact};

pub(crate) const BOOT: &str = "private-boot";

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

pub(crate) fn per_core(values: &[i64]) -> PerCore<TargetOffset> {
    values
        .iter()
        .enumerate()
        .map(|(n, &value)| {
            (
                core(i64::try_from(n).unwrap()),
                TargetOffset::new(value).unwrap(),
            )
        })
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
        fixes: 0,
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

pub(crate) fn lock(fs: &MemFs) -> StateDir {
    StateDir::simulated(fs.clone()).unwrap()
}

/// Writes session journals into an in-memory state directory.
pub(crate) struct Writer {
    journal: Journal,
    second: i64,
    boot: u32,
}

impl Writer {
    pub(crate) fn open(fs: &MemFs) -> Self {
        Self::reopen(lock(fs), 1, 0)
    }

    fn reopen(dir: StateDir, boot: u32, second: i64) -> Self {
        let journal = dir.open(BootId::from(format!("{BOOT}-{boot}"))).unwrap();
        Self {
            journal,
            second,
            boot,
        }
    }

    pub(crate) fn add(&mut self, payload: impl Into<Payload>) -> Seq {
        self.second += 1;
        let stamp = Stamp {
            time: Timestamp::from_second(1_700_000_000 + self.second).unwrap(),
            mono: None,
        };
        self.journal.append(stamp, payload.into(), &[]).unwrap().seq
    }

    /// `session.start` of `cores` under the `fixture` BIOS context, then its `session.context`.
    pub(crate) fn begin(&mut self, session: &str, ruleset: u32, cores: &[i64]) {
        self.begin_with(session, cores, |build| build.ruleset = ruleset);
    }

    /// [`Self::begin`] with the recorded build adjusted by `adjust`.
    pub(crate) fn begin_with(
        &mut self,
        session: &str,
        cores: &[i64],
        adjust: impl FnOnce(&mut Build),
    ) {
        let mut build = Build {
            version: "fixture".into(),
            rev: Some("abc".into()),
            ruleset: 1,
            schema: SCHEMA,
            fixes: 0,
            evidence: 1,
        };
        adjust(&mut build);
        self.add(SessionStart {
            build,
            session: session.parse().unwrap(),
            cores: cores
                .iter()
                .map(|&n| CoreInfo {
                    core: core(n),
                    ccd: CcdId(0),
                    cpus: vec![CpuId(u16::try_from(n).unwrap())],
                })
                .collect(),
        });
        self.add(SessionContext::from(bios("fixture")));
    }

    pub(crate) fn trial(&mut self, intent: TrialIntent, end: TrialEnd) -> Seq {
        self.add(intent);
        self.add(end)
    }

    pub(crate) fn close(self) -> StateDir {
        self.journal.close().unwrap()
    }

    /// Archives the session and starts an empty journal for the next one.
    pub(crate) fn archive(self) -> Self {
        let Self {
            journal,
            second,
            boot,
        } = self;
        let stamp = Stamp {
            time: Timestamp::from_second(1_700_000_000 + second + 1).unwrap(),
            mono: None,
        };
        let dir = journal
            .archive(stamp, ArchiveKind::Transition, &[])
            .unwrap();
        Self::reopen(dir, boot + 1, second + 2)
    }
}

pub(crate) fn alone(id: &str, core_n: i64, value: i64, profile: &[i64]) -> TrialIntent {
    TrialIntent {
        core: Some(core(core_n)),
        offset: Some(TargetOffset::new(value).unwrap()),
        ..together(id, Regime::R1, &[], profile, Condition::Alone)
    }
}

pub(crate) fn together(
    id: &str,
    regime: Regime,
    cores: &[i64],
    profile: &[i64],
    condition: Condition,
) -> TrialIntent {
    TrialIntent {
        trial: TrialId::from(id.to_owned()),
        kernel_cursor: None,
        kernel_error: None,
        profile: per_core(profile),
        core: None,
        cores: set(cores),
        offset: None,
        regime,
        workload: workload(regime, 0),
        duration_s: 90,
        condition,
        activity: Some(Activity::Search),
        retry: false,
        cycle: None,
        step: None,
        hunt: None,
        group: None,
        round: None,
        rerun: false,
    }
}

pub(crate) fn end(id: &str, outcome: Outcome, duration_s: u32) -> TrialEnd {
    TrialEnd {
        trial: TrialId::from(id.to_owned()),
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

pub(crate) fn failed(id: &str, signal: Signal, duration_s: u32) -> TrialEnd {
    TrialEnd {
        signal: Some(signal),
        ..end(id, Outcome::Failure, duration_s)
    }
}
