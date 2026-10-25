//! The night query: what the night does next.

use std::collections::BTreeMap;

use jiff::Timestamp;
use togi_journal::Seq;
use togi_machine::{Backend, CcdProfileError, ContentId, WorkloadId};

use super::State;
use super::decision::Decision;
use super::plan::TrialPlan;
use crate::day::DayStepError;
use crate::night::{Cell, Layout, MatrixError};

/// The frozen content identity of every workload the night may run, keyed by the workload with its run
/// count normalized away (the identity names the workload, not how many copies run).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Contents(BTreeMap<WorkloadId, ContentId>);

impl Contents {
    #[must_use]
    pub fn new(entries: impl IntoIterator<Item = (WorkloadId, ContentId)>) -> Self {
        Self(
            entries
                .into_iter()
                .map(|(workload, content)| (workload.runs(), content))
                .collect(),
        )
    }

    pub(super) const fn map(&self) -> &BTreeMap<WorkloadId, ContentId> {
        &self.0
    }
}

/// What the night query asks. The query continues the open night, or opens one when none is open; a night
/// has no deadline and closes at the operator's stop, a later boot's day start or a session reset.
#[derive(Clone, Copy, Debug)]
pub struct NightQuery<'a> {
    pub now: Timestamp,
    pub contents: &'a Contents,
    /// The backends excluded for the rest of the night, each with the events that excluded it (its
    /// inconclusive streak): no planned cell runs a workload on one, and the night's plan records them.
    pub unavailable: &'a BTreeMap<Backend, Vec<Seq>>,
}

/// Whether every workload of `cell` runs on a backend not in `unavailable`.
pub(super) fn runnable(cell: &Cell, unavailable: &BTreeMap<Backend, Vec<Seq>>) -> bool {
    super::plan::workloads_of(cell.workloads)
        .iter()
        .all(|workload| !unavailable.contains_key(&workload.backend()))
}

/// What the night does next.
#[derive(Clone, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "commitments are built, journaled and dropped at once, never stored in bulk"
)]
pub enum Action {
    /// Journal this commitment, fold it and ask again.
    Decide(Decision),
    /// Run this trial.
    RunTrial(TrialPlan),
    /// The night is over.
    EndNight(EndReason),
}

/// Why the policy ends a night. Otherwise a night runs until it closes, and the query always has a trial
/// to run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndReason {
    /// A conclusive failure at all-zero ended the run.
    DeadEnd,
    /// Every cell the night could run, the filler's included, needs an excluded backend.
    Unavailable,
}

/// Why the state cannot answer the query yet.
#[derive(Debug, thiserror::Error)]
pub enum NotReady {
    #[error("deciding a night before the session started")]
    NoSession,
    #[error("deciding a night before a firmware context was reported")]
    NoFirmware,
    #[error("deciding a night before the day baseline")]
    NoBaseline,
    #[error("naming a workload's content")]
    UnknownContent(WorkloadId),
    #[error("building a coverage matrix")]
    Matrix(#[from] MatrixError),
    #[error("building a profile")]
    Profile(#[from] CcdProfileError),
    #[error("building a journal number")]
    Number(#[from] togi_journal::InvalidNumber),
    #[error("hashing a coverage matrix")]
    Encode(#[from] serde_json::Error),
    #[error("recomputing the cell of a recorded trial")]
    UnmatchedCell,
    #[error("reading a recorded core profile")]
    CoreProfile(#[from] crate::day::CoreProfileError),
    #[error("deciding a day step")]
    Day(#[from] DayStepError),
    #[error("folding an owed commitment")]
    Fold(#[from] super::fold::FoldError),
    #[error("numbering an owed commitment past the last sequence number")]
    SeqExhausted,
}

impl State {
    /// Runs `f` on the machine's coverage layout.
    pub(super) fn with_layout<R>(
        &self,
        f: impl FnOnce(&Layout<'_>) -> Result<R, NotReady>,
    ) -> Result<R, NotReady> {
        self.layout_at(None, f)
    }

    /// Runs `f` on the coverage layout the journal up to `cutoff` (all of it for `None`) derives.
    pub(super) fn layout_at<R>(
        &self,
        cutoff: Option<Seq>,
        f: impl FnOnce(&Layout<'_>) -> Result<R, NotReady>,
    ) -> Result<R, NotReady> {
        let topology = self.machine.topology.as_ref().ok_or(NotReady::NoSession)?;
        let partials = self.chains(cutoff, topology);
        f(&Layout {
            topology,
            cores: &self.machine.cores,
            partials: &partials,
        })
    }
}

impl State {
    /// Each CCD's honest partial chain, from the measured requests of the latest full-CCD R7 load that
    /// ended at or before `cutoff` under the current firmware. A CCD with no such load, or one whose
    /// load carries no finite request for a core, derives the explicit-subset fallback: the chain
    /// then says which cores had no request, and the matrix lists what no chain can supply.
    fn chains(
        &self,
        cutoff: Option<Seq>,
        topology: &crate::Topology,
    ) -> BTreeMap<togi_machine::CcdId, crate::HonestChain> {
        topology
            .ccds()
            .iter()
            .map(|&(ccd, loaded)| {
                let requests = self
                    .trials
                    .loads
                    .iter()
                    .rev()
                    .filter_map(|&at| self.trials.all.get(at))
                    .filter(|trial| {
                        trial.intent.cores == loaded
                            && trial.firmware.as_ref() == self.machine.firmware.as_ref()
                    })
                    .filter_map(|trial| trial.end.as_ref())
                    .find(|ended| cutoff.is_none_or(|cutoff| ended.seq <= cutoff))
                    .map(|ended| ended.end.voltage_requests_v)
                    .unwrap_or_default();
                (ccd, crate::HonestChain::derive(loaded, &requests))
            })
            .collect()
    }
}
