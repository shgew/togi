//! The composed Ruleset 13 tuner state (`docs/spec/tuner.md`, `docs/spec/runtime.md`, Ruleset 13 policy
//! boundary).
//!
//! [`State`] folds the journal: raw facts (session, trial, day observer and answer records) and the
//! policy's own commitments (exposure, edge, cap, night plan, qualification, reproduction, day event,
//! veto, interval, hold, step and application). Its queries are pure: [`State::next`] decides what the
//! night does next, [`State::day`] what a day invocation must commit, and [`State::project`] what
//! status shows. A query reads nothing but the state and its arguments. The state's only interior
//! mutability is its memoized query caches (`cache.rs`), which are never state, so cloning it, folding
//! synthesized outcomes into the clone and querying again still previews a night without touching the
//! journal.
//!
//! Every derived commitment is owed once per identity: a query returns it until the journal carries it,
//! so appending the returned decision and folding it makes the next query move on, and replaying the
//! whole journal reaches the same state a live fold reaches.

mod books;
mod cache;
mod climb_projection;
mod controls;
mod day_events;
mod day_query;
mod decision;
mod evidence;
mod filler;
mod fold;
mod ledger;
mod mapping;
mod night_next;
mod night_query;
mod permission;
mod plan;
mod projection;
mod qualify;
mod reproduce;
mod statistical;
pub mod successor;
mod wire;

use togi_machine::CcdId;

pub use climb_projection::{
    ClimbCampaignProjection, ClimbDailyEvidence, ClimbEventProjection, ClimbGoalSupport,
    ClimbHoldProjection, ClimbPermissionMode, ClimbProfiles, ClimbProjection,
    ClimbProposalProjection, ClimbQualificationProjection, ClimbQualificationState,
    ClimbStepProjection, ClimbUnitProjection, ClimbUnitState, DiagnosticStatisticalProjection,
};
pub use day_query::DayQuery;
pub use decision::{Decision, PolicyPayload};
pub use fold::FoldError;
pub use night_query::{Action, Contents, EndReason, NightQuery, NotReady};
pub use plan::{PlanError, TrialPlan};
pub use projection::{
    CapProjection, DayProjection, EventProjection, FirmwareValidity, HoldProjection,
    MappingProjection, NightProjection, Projection, QualificationProgress, QualificationProjection,
    QualificationStage, ReproductionProjection, UnitProjection, VetoProjection,
};

use crate::cap::{Beta, Rate};
use crate::day::DayParams;
use crate::night::Budgets;
use books::{Days, Ledger, Machine, Nights, Trials};
use togi_journal::Seq;

/// The thresholds the policy decides by, explicit so a simulator can vary them.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Params {
    /// The slope projected with before any class has failures at two offsets.
    pub fallback_slope: Beta,
    /// λ*: the machine's incremental hazard budget.
    pub machine_budget: Rate,
    /// The day hold.
    pub hold: DayParams,
    /// The night scheduler's soft shares of a night's elapsed observed time.
    pub budgets: Budgets,
}

impl Params {
    /// The specification's values.
    pub const SPEC: Self = Self {
        fallback_slope: Beta::FALLBACK,
        machine_budget: Rate::MACHINE_BUDGET,
        hold: DayParams::SPEC,
        budgets: Budgets {
            mapping: crate::night::MAPPING_SHARE_PCT,
            controls: crate::night::CONTROLS_SHARE_PCT,
            requalification: crate::night::REQUALIFICATION_SHARE_PCT,
            reproduction: crate::night::REPRODUCTION_SHARE_PCT,
            qualification: crate::night::QUALIFICATION_SHARE_PCT,
        },
    };
}

/// A conclusive failure at all-zero ended the run; it stands until an explicit reset.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Stop {}

/// The folded Ruleset 13 policy state.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    params: Params,
    /// The last folded position; earlier events replay as no-ops.
    last: Option<Seq>,
    machine: Machine,
    trials: Trials,
    ledger: Ledger,
    nights: Nights,
    days: Days,
    stop: Option<Stop>,
    #[serde(skip)]
    caches: Caches,
}

/// A cap outcome with what it was derived from.
type CapCache = std::collections::BTreeMap<
    togi_machine::CcdId,
    (statistical::CapKey, std::sync::Arc<statistical::CapOutcome>),
>;

/// A cap's journal payload with the outcome, committed cap and edge it was built from.
type PayloadCache = std::collections::BTreeMap<
    togi_machine::CcdId,
    (
        std::sync::Arc<statistical::CapOutcome>,
        Option<Seq>,
        togi_journal::Edge,
        std::sync::Arc<Option<togi_journal::CcdCap>>,
    ),
>;

/// The memoized derivations of one state.
#[derive(Clone, Debug, Default, PartialEq)]
struct Caches {
    mapping: cache::Cache<mapping::MapReplay>,
    payloads: cache::Cache<PayloadCache>,
    caps: cache::Cache<CapCache>,
    records: cache::Cache<qualify::RecordCache>,
    statuses: cache::Cache<qualify::StatusCache>,
    filler_failures: cache::Cache<day_events::FillerFailures>,
    nights: cache::Cache<night_next::NightTallies>,
    intervals: cache::Cache<day_query::IntervalCache>,
    warnings: cache::Cache<ledger::Monotonicity>,
}

impl State {
    /// An empty state; fold the journal from `session.start` on.
    #[must_use]
    pub fn new(params: Params) -> Self {
        Self {
            params,
            last: None,
            machine: Machine::default(),
            trials: Trials::default(),
            ledger: Ledger::default(),
            nights: Nights::default(),
            days: Days::default(),
            stop: None,
            caches: Caches::default(),
        }
    }

    #[must_use]
    pub const fn params(&self) -> &Params {
        &self.params
    }

    /// The CCDs of the machine, ascending; empty before `session.start`.
    pub(crate) fn ccds(&self) -> Vec<CcdId> {
        self.machine
            .topology
            .as_ref()
            .map(|topology| topology.ccds().iter().map(|&(ccd, _)| ccd).collect())
            .unwrap_or_default()
    }
}
