use std::path::PathBuf;
use std::time::Duration;

use crate::id::ScopeName;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Stat {
    pub(crate) state: char,
    pub(crate) pgrp: i64,
    pub(crate) utime: u64,
    pub(crate) stime: u64,
    pub(crate) starttime: u64,
    pub(crate) processor: u32,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum StatError {
    #[error("malformed /proc stat: {0}")]
    Malformed(String),
}

/// Splits after the last `)` so a command name may hold spaces and parentheses.
pub(crate) fn parse_stat(text: &str) -> Result<Stat, StatError> {
    let _ = text;
    unimplemented!()
}

/// Cumulative CPU time from utime and stime ticks, checked.
pub(crate) fn cpu_time(stat: &Stat, ticks_per_second: u64) -> Option<Duration> {
    let _ = (stat, ticks_per_second);
    unimplemented!()
}

pub(crate) fn ticks_per_second() -> u64 {
    unimplemented!()
}

/// Whether some cgroup line ends in `/<scope>.scope`.
pub(crate) fn in_scope(cgroup: &str, scope: &ScopeName) -> bool {
    let _ = (cgroup, scope);
    unimplemented!()
}

/// The last trial scope named by a path component of any cgroup line.
pub(crate) fn scope_of(cgroup: &str) -> Option<ScopeName> {
    let _ = cgroup;
    unimplemented!()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Usage {
    pub(crate) cpu_time: Duration,
    pub(crate) cpu: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Thread {
    pub(crate) tid: u32,
    pub(crate) cpu: u32,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProcError {
    /// The process or thread vanished while being read: normal, never a lost observation.
    #[error("process disappeared")]
    Gone,
    #[error("{0}")]
    Observation(String),
}

/// What a thread listing found: readable threads stay evidence even when others failed.
#[derive(Debug, Default)]
pub(crate) struct Threads {
    pub(crate) threads: Vec<Thread>,
    pub(crate) errors: Vec<ProcError>,
}

/// A process seen in a trial scope, identified by pid and start time and never signalled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScopeProcess {
    pub(crate) scope: ScopeName,
    pub(crate) pid: u32,
    pub(crate) starttime: u64,
}

/// Reads `/proc`, rooted anywhere so fixtures can stand in.
#[derive(Clone, Debug)]
pub(crate) struct Proc {
    root: PathBuf,
}

impl Proc {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn usage(&self, pid: u32) -> Result<Usage, ProcError> {
        let _ = (&self.root, pid);
        unimplemented!()
    }

    pub(crate) fn threads(&self, pid: u32) -> Result<Threads, ProcError> {
        let _ = pid;
        unimplemented!()
    }

    pub(crate) fn in_scope(&self, pid: u32, scope: &ScopeName) -> Result<bool, ProcError> {
        let _ = (pid, scope);
        unimplemented!()
    }

    pub(crate) fn scope_processes(&self) -> Result<Vec<ScopeProcess>, ProcError> {
        unimplemented!()
    }

    /// Alive means the same start time and a state other than zombie or dead.
    pub(crate) fn alive(&self, process: &ScopeProcess) -> Result<bool, ProcError> {
        let _ = process;
        unimplemented!()
    }
}
