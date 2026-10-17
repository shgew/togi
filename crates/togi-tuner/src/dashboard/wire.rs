//! The `state.json` shape of the projection views whose absent quotas and search strides the
//! tuner holds as variants: a count and length of zero for an absent quota, and step zero with
//! `confirm` for a confirmation turn.

use std::num::NonZeroU8;

use serde::{Deserialize, Serialize};
use togi_machine::{CcdId, CoreId, CoreSet, Regime, TargetOffset, WorkloadId};

use super::{Lengths, Quota, TurnKind};

#[expect(
    clippy::struct_excessive_bools,
    reason = "the flat shape the dashboard view is written in"
)]
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct CyclePart {
    cores: CoreSet,
    ccd: Option<CcdId>,
    full: bool,
    partial: bool,
    short: u32,
    short_s: u32,
    long: u32,
    long_s: u32,
    passed: u32,
    failed: u32,
    done: bool,
    running: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SearchTurn {
    core: CoreId,
    confirm: bool,
    regimes: Vec<Regime>,
    offset: TargetOffset,
    workload: WorkloadId,
    step: u8,
    light: u32,
    heavy: u32,
    needed: u32,
    running: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct RerunPlan {
    regime: Regime,
    cores: CoreSet,
    short: u32,
    short_s: u32,
    long: u32,
    long_s: u32,
}

fn flat(quota: Option<Quota>) -> (u32, u32) {
    quota.map_or((0, 0), |quota| (quota.count, quota.secs))
}

fn quota(count: u32, secs: u32) -> Option<Quota> {
    (count > 0).then_some(Quota { count, secs })
}

impl From<super::CyclePart> for CyclePart {
    fn from(part: super::CyclePart) -> Self {
        let (short, short_s) = flat(part.lengths.map(|lengths| lengths.short));
        let (long, long_s) = flat(part.lengths.and_then(|lengths| lengths.long));
        Self {
            cores: part.cores,
            ccd: part.ccd,
            full: part.full,
            partial: part.partial,
            short,
            short_s,
            long,
            long_s,
            passed: part.passed,
            failed: part.failed,
            done: part.done,
            running: part.running,
        }
    }
}

impl From<CyclePart> for super::CyclePart {
    fn from(part: CyclePart) -> Self {
        Self {
            cores: part.cores,
            ccd: part.ccd,
            full: part.full,
            partial: part.partial,
            lengths: quota(part.short, part.short_s).map(|short| Lengths {
                short,
                long: quota(part.long, part.long_s),
            }),
            passed: part.passed,
            failed: part.failed,
            done: part.done,
            running: part.running,
        }
    }
}

impl From<super::SearchTurn> for SearchTurn {
    fn from(turn: super::SearchTurn) -> Self {
        let (confirm, step) = match turn.kind {
            TurnKind::Confirm => (true, 0),
            TurnKind::Search { step } => (false, step.get()),
        };
        Self {
            core: turn.core,
            confirm,
            regimes: turn.regimes,
            offset: turn.offset,
            workload: turn.workload,
            step,
            light: turn.light,
            heavy: turn.heavy,
            needed: turn.needed,
            running: turn.running,
        }
    }
}

impl TryFrom<SearchTurn> for super::SearchTurn {
    type Error = &'static str;

    fn try_from(turn: SearchTurn) -> Result<Self, Self::Error> {
        let kind = match (turn.confirm, NonZeroU8::new(turn.step)) {
            (true, None) => TurnKind::Confirm,
            (false, Some(step)) => TurnKind::Search { step },
            (true, Some(_)) => return Err("a confirmation turn has no search step"),
            (false, None) => return Err("a search turn has a step of at least one count"),
        };
        Ok(Self {
            core: turn.core,
            kind,
            regimes: turn.regimes,
            offset: turn.offset,
            workload: turn.workload,
            light: turn.light,
            heavy: turn.heavy,
            needed: turn.needed,
            running: turn.running,
        })
    }
}

impl From<super::RerunPlan> for RerunPlan {
    fn from(plan: super::RerunPlan) -> Self {
        let (long, long_s) = flat(plan.lengths.long);
        Self {
            regime: plan.regime,
            cores: plan.cores,
            short: plan.lengths.short.count,
            short_s: plan.lengths.short.secs,
            long,
            long_s,
        }
    }
}

impl TryFrom<RerunPlan> for super::RerunPlan {
    type Error = &'static str;

    fn try_from(plan: RerunPlan) -> Result<Self, Self::Error> {
        let short = quota(plan.short, plan.short_s).ok_or("a rerun runs at least one trial")?;
        Ok(Self {
            regime: plan.regime,
            cores: plan.cores,
            lengths: Lengths {
                short,
                long: quota(plan.long, plan.long_s),
            },
        })
    }
}
