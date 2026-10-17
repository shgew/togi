use std::collections::BTreeMap;

use jiff::Timestamp;
use togi_journal::{
    Build, MemFs, Outcome, Payload, Recording, SCHEMA, SessionContext, SessionStart, Stamp,
    StateDir, TrialEnd, TrialId, TrialIntent,
};
use togi_machine::{
    BiosContext, BootId, CcdId, Condition, CoreId, CoreInfo, CoreSet, CpuId, PerCore, Regime,
    TargetOffset, WorkloadId,
};

pub(crate) const SESSION: &str = "20260101T000000Z";

fn core(n: i64) -> CoreId {
    CoreId::new(n).unwrap()
}

fn set(cores: &[i64]) -> CoreSet {
    cores.iter().map(|&n| core(n)).collect()
}

fn append(dir: &mut togi_journal::Journal, second: &mut i64, payload: impl Into<Payload>) {
    *second += 1;
    let stamp = Stamp {
        time: Timestamp::from_second(1_700_000_000 + *second).unwrap(),
        mono: None,
    };
    dir.append(stamp, payload.into(), &[]).unwrap();
}

fn start(cores: &[i64]) -> SessionStart {
    SessionStart {
        build: Build {
            version: "fixture".into(),
            rev: Some("abc".into()),
            ruleset: 6,
            schema: SCHEMA,
            fixes: 0,
            evidence: 1,
        },
        session: SESSION.parse().unwrap(),
        cores: cores
            .iter()
            .map(|&n| CoreInfo {
                core: core(n),
                ccd: CcdId::new(0).unwrap(),
                cpus: vec![CpuId::from(u16::try_from(n).unwrap())],
            })
            .collect(),
    }
}

fn intent() -> TrialIntent {
    TrialIntent {
        trial: TrialId::from("1".to_owned()),
        kernel_cursor: None,
        kernel_error: None,
        profile: [
            (core(0), TargetOffset::new(-10).unwrap()),
            (core(1), TargetOffset::new(0).unwrap()),
        ]
        .into_iter()
        .collect(),
        core: Some(core(0)),
        cores: CoreSet::default(),
        offset: Some(TargetOffset::new(-10).unwrap()),
        regime: Regime::R1,
        workload: WorkloadId::new(Regime::R1, 0).unwrap(),
        duration_s: 90,
        condition: Condition::Alone,
        activity: Some(togi_journal::Activity::Search),
        retry: false,
        cycle: None,
        step: None,
        hunt: None,
        group: None,
        round: None,
        rerun: false,
    }
}

fn end() -> TrialEnd {
    TrialEnd {
        trial: TrialId::from("1".to_owned()),
        kernel_cursor: None,
        kernel_error: None,
        outcome: Outcome::Pass,
        signal: None,
        core: None,
        duration_s: 90,
        tctl_max_c: None,
        voltage_request_median_v: None,
        voltage_request_min_v: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: set(&[]),
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

fn bios() -> BiosContext {
    BiosContext {
        bios_version: "fixture".into(),
        board: "fixture board".into(),
        cpu_model: "fixture cpu".into(),
        microcode: "0x1".into(),
        boost_limit_mhz: 5000,
    }
}

/// An in-memory state directory holding one session of two cores and optionally one passed trial.
pub(crate) fn state(trial: bool) -> (MemFs, StateDir) {
    let fs = MemFs::new();
    #[expect(
        clippy::disallowed_methods,
        reason = "content fixtures keep the journal in memory"
    )]
    let dir = StateDir::simulated(fs.clone()).unwrap();
    let mut journal = dir.open(BootId::from("fixture-boot".to_owned())).unwrap();
    let mut second = 0;
    append(&mut journal, &mut second, start(&[0, 1]));
    append(&mut journal, &mut second, SessionContext::from(bios()));
    if trial {
        append(&mut journal, &mut second, intent());
        append(&mut journal, &mut second, end());
    }
    let dir = journal.close().unwrap();
    (fs, dir)
}

pub(crate) fn recordings(trial: bool) -> Vec<Recording> {
    let (_, dir) = state(trial);
    dir.recordings_lenient().unwrap()
}

pub(crate) fn unknown_recording() -> Recording {
    let (fs, dir) = state(true);
    let path = dir.path().join("events.jsonl");
    let text = Recording::read_in(&fs, &path)
        .unwrap()
        .events()
        .iter()
        .map(|event| event.encode().unwrap())
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    for (index, text) in text.iter().enumerate() {
        let mut line: serde_json::Value = serde_json::from_str(text).unwrap();
        if index >= 2 {
            line["seq"] = (line["seq"].as_u64().unwrap() + 1).into();
        }
        lines.push(line.clone());
        if index == 1 {
            line["seq"] = 3.into();
            line["kind"] = "future.thing".into();
            line["data"] = serde_json::json!({});
            lines.push(line);
        }
    }
    let spliced: String = lines.iter().map(|line| line.to_string() + "\n").collect();
    fs.put(&path, spliced.as_bytes());
    Recording::read_in_lenient(&fs, &path).unwrap()
}
