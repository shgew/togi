use std::path::PathBuf;

use jiff::Timestamp;
use togi_machine::{BootId, CcdId, CoreId, CoreInfo, CpuId};

use crate::dir::{LockError, StateDir};
use crate::fs::MemFs;
use crate::{Build, Event, Journal, Payload, SCHEMA, SessionId, SessionStart, SessionWarning, Stamp};

#[expect(clippy::disallowed_methods, reason = "the journal's own tests run in memory")]
pub(crate) fn try_simulated(fs: &MemFs) -> Result<StateDir, LockError> {
    StateDir::simulated(fs.clone())
}

pub(crate) fn simulated(fs: &MemFs) -> StateDir {
    try_simulated(fs).unwrap()
}

pub(crate) fn boot(n: u32) -> BootId {
    BootId::from(format!("boot-{n}"))
}

pub(crate) fn stamp(second: i64) -> Stamp {
    Stamp { time: Timestamp::from_second(1_700_000_000 + second).unwrap(), mono: None }
}

pub(crate) fn session(text: &str) -> SessionId {
    text.parse().unwrap()
}

pub(crate) fn default_session() -> SessionId {
    session("20231114T221320Z")
}

pub(crate) fn start(session: SessionId) -> Payload {
    Payload::SessionStart(SessionStart {
        build: Build { version: "0.1.0".into(), rev: None, ruleset: 1, schema: SCHEMA, fixes: 0, evidence: 1 },
        session,
        cores: vec![CoreInfo { core: CoreId::new(0).unwrap(), ccd: CcdId(0), cpus: vec![CpuId(0)] }],
    })
}

pub(crate) fn note(text: &str) -> Payload {
    Payload::SessionWarning(SessionWarning { operation: text.into(), trial: None, error: String::new() })
}

pub(crate) fn begin(fs: &MemFs, session: SessionId) -> Journal {
    let mut journal = simulated(fs).open(boot(1)).unwrap();
    journal.append(stamp(0), start(session), &[]).unwrap();
    journal
}

pub(crate) fn begin_default(fs: &MemFs) -> Journal {
    begin(fs, default_session())
}

pub(crate) fn reopen(fs: &MemFs, boot_n: u32) -> Journal {
    simulated(fs).open(boot(boot_n)).unwrap()
}

pub(crate) fn live(fs: &MemFs) -> Vec<u8> {
    fs.contents("/state/events.jsonl").unwrap_or_default()
}

pub(crate) fn kinds(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(|event| event.kind().as_str()).collect()
}

pub(crate) fn path(name: &str) -> PathBuf {
    PathBuf::from("/state").join(name)
}
