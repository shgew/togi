use std::collections::HashSet;

use togi_config::Backends;
use togi_facts::{BackendIdentity, Decisive, Fact, FactId, Observation};
use togi_journal::{CarriedCore, CarriedSource, Payload, SessionCarried};
use togi_machine::{BiosContext, CoreSet};

use crate::select::PendingCarry;

/// What the transition carries into the session, final once resolved against its actual BIOS context, backends and
/// evidence epoch.
///
/// The session appends it in this order (C9): after `session.context`, the baseline and its notice and before any
/// phase, [`fact_payloads`](Self::fact_payloads), then [`session_carried`](Self::session_carried) with no cause as
/// the commitment. The carry marker clears only after that.
///
/// Initial seed precedence (C10) stays in the session: configured candidate solo limit, configured start offset,
/// carried candidate solo limit, then the clamped baseline. A candidate starts search in `check_solo_limit` with
/// frozen first R1 and R2 workloads. A nonzero failure point `m` clamps a start at or deeper than `m` to `m + 1`.
/// A failure point at 0 was confirmed before it was written, and stays a dead end until reset.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCarry {
    sources: Vec<CarriedSource>,
    failure_points: bool,
    detail: Option<String>,
    seeds: Vec<CarriedCore>,
    facts: Vec<Fact>,
}

impl PendingCarry {
    /// Resolves against the session's recorded BIOS context. Failure points and facts carry only when the newest
    /// source recorded that same context; otherwise failure points are cleared, cores with no candidate solo limit
    /// dropped, and no fact carries. A pass also needs the current evidence `epoch` and the identity of the
    /// current backend of its workload.
    pub fn resolve(&self, current: &BiosContext, backends: &Backends, epoch: u32) -> ResolvedCarry {
        let (failure_points, detail) = match &self.context {
            None => (
                false,
                Some("the archived session recorded no BIOS context".to_owned()),
            ),
            Some(recorded) => match recorded.first_difference(current) {
                None => (true, None),
                Some(mismatch) => (false, Some(mismatch.to_string())),
            },
        };
        let seeds = if failure_points {
            self.seeds.clone()
        } else {
            self.seeds
                .iter()
                .filter(|seed| seed.candidate_solo_limit.is_some())
                .map(|seed| CarriedCore {
                    failure_point: None,
                    ..seed.clone()
                })
                .collect()
        };
        let facts = if failure_points {
            self.facts
                .iter()
                .filter(|fact| keeps(fact, backends, epoch))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        ResolvedCarry {
            sources: self.sources.clone(),
            failure_points,
            detail,
            seeds,
            facts,
        }
    }
}

fn keeps(fact: &Fact, backends: &Backends, epoch: u32) -> bool {
    let Observation::Trial {
        class,
        outcome: Decisive::Pass,
        ..
    } = &fact.observation
    else {
        return true;
    };
    let backend = class.workload.backend();
    let current = backends.path(backend).map(|path| BackendIdentity {
        backend,
        path: path.to_string_lossy().into_owned(),
    });
    fact.source.epoch == epoch && fact.source.backend == current
}

/// The original identity of a copied fact an event records, if it is one.
pub fn copied_identity(payload: &Payload) -> Option<FactId> {
    let source = match payload {
        Payload::TrialCarried(carried) => &carried.source,
        Payload::FailureCarried(carried) => &carried.source,
        Payload::SessionStart(_)
        | Payload::SessionContext(_)
        | Payload::SessionBaseline(_)
        | Payload::SessionNotice(_)
        | Payload::SessionWarning(_)
        | Payload::SessionArchived(_)
        | Payload::SessionCarried(_)
        | Payload::ConfigLoaded(_)
        | Payload::PreflightCheck(_)
        | Payload::SmuIntent(_)
        | Payload::SmuWrite(_)
        | Payload::SmuReadback(_)
        | Payload::SmuError(_)
        | Payload::ProfileApplied(_)
        | Payload::ProfileChange(_)
        | Payload::ProfileRestored(_)
        | Payload::TrialIntent(_)
        | Payload::TrialStart(_)
        | Payload::TrialProgress(_)
        | Payload::TrialSignal(_)
        | Payload::TrialSample(_)
        | Payload::TrialEnd(_)
        | Payload::Failure(_)
        | Payload::Mce(_)
        | Payload::CrashDetected(_)
        | Payload::TunerDecision(_)
        | Payload::CorePhase(_)
        | Payload::CheckingCycle(_)
        | Payload::CheckingStep(_)
        | Payload::CheckingChain(_)
        | Payload::HostRanking(_)
        | Payload::HuntStart(_)
        | Payload::HuntGroup(_)
        | Payload::HuntEnd(_)
        | Payload::HuntSkipped(_)
        | Payload::Combination(_)
        | Payload::DeepeningRound(_)
        | Payload::TunerWarning(_)
        | Payload::BackendRetry(_)
        | Payload::CommandReset(_)
        | Payload::DefectFound(_)
        | Payload::DefectAnswered(_)
        | Payload::DeadEnd(_)
        | Payload::BootSavedEntry(_)
        | Payload::BootLeaveReason(_)
        | Payload::Shutdown(_)
        | Payload::JournalTorn(_)
        | Payload::DayBootStart(_)
        | Payload::DayCheckpoint(_)
        | Payload::DayOffsetsChanged(_)
        | Payload::DayCleanStop(_)
        | Payload::DayAnswer(_)
        | Payload::DayFault(_)
        | Payload::DayUncleanEnd(_)
        | Payload::StateRebuilt(_) => return None,
    };
    Some(FactId {
        session: source.session,
        seq: source.seq,
    })
}

impl ResolvedCarry {
    pub fn sources(&self) -> &[CarriedSource] {
        &self.sources
    }

    /// False, with [`detail`](Self::detail) saying why, when failure points stay behind.
    pub const fn failure_points(&self) -> bool {
        self.failure_points
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Per core, ascending, the carried candidate solo limit and failure point.
    pub fn seeds(&self) -> &[CarriedCore] {
        &self.seeds
    }

    /// The eligible facts, chronological by original session then sequence.
    pub fn facts(&self) -> &[Fact] {
        &self.facts
    }

    /// The `session.carried` commitment, limited to the cores of this machine.
    pub fn session_carried(&self, cores: CoreSet) -> SessionCarried {
        SessionCarried {
            sources: self.sources.clone(),
            failure_points: self.failure_points,
            detail: self.detail.clone(),
            carried: self
                .seeds
                .iter()
                .filter(|seed| cores.contains(seed.core))
                .cloned()
                .collect(),
        }
    }

    /// The `trial.carried` and `failure.carried` payloads whose original identity `copied` lacks, in order, for
    /// facts loading only cores of this machine.
    pub fn fact_payloads(&self, copied: &HashSet<FactId>, cores: CoreSet) -> Vec<Payload> {
        self.facts
            .iter()
            .filter(|fact| !copied.contains(&fact.id()))
            .filter(|fact| fact.loaded().iter().all(|core| cores.contains(core)))
            .map(Fact::to_payload)
            .collect()
    }
}
