use std::collections::HashMap;
use std::fmt;

use togi_machine::{CcdId, CoreId};

use togi_journal::Seq;

use super::Applied;
use crate::{Depth, LoadKey, Profile, Topology};

/// Voltage-targeted backoffs of one load, since its latest pass, after which its next
/// unattributed failure is located on the unloaded cores.
pub(crate) const ESCALATE_AFTER: u32 = 2;

/// What a backoff decision answered. Only a failure that charged the loaded cores by request
/// order counts toward escalation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackoffKind {
    /// A failure naming no core, or one routed as unattributed: a named CO-0 core on a CCD with
    /// no loaded core, or a failure whose all-zero rerun passed.
    Unattributed,
    /// A failure charged to the core it names, as a loaded or routed-to-its-CCD named core.
    Named,
    /// The located hunt's backoff that charges a named core.
    LocatedNamed,
    /// A phase-2 blame or confirmation revert.
    Deepening,
    /// A held later failure, which moved no voltage.
    Hold,
}

impl BackoffKind {
    pub(crate) const fn counts(self) -> bool {
        matches!(self, Self::Unattributed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Strike {
    decision: Seq,
    failure: Seq,
}

/// Why a failure is located instead of backed off.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Escalation {
    /// Every loaded core on the affected CCDs is at CO 0 while an unloaded core is not: no loaded
    /// core can step back.
    LoadedAtZero,
    /// The load was backed off `failures` times since its last pass, by these decisions.
    Backoffs { failures: u32, decisions: Vec<Seq> },
}

impl fmt::Display for Escalation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoadedAtZero => f.write_str(
                "every loaded core of the affected CCDs is at CO 0 while an unloaded core is not, \
                 so backing off the loaded cores cannot help",
            ),
            Self::Backoffs {
                failures,
                decisions,
            } => {
                write!(
                    f,
                    "this load has been backed off {failures} times since its last passing trial ("
                )?;
                for (at, decision) in decisions.iter().enumerate() {
                    if at > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "#{}", decision.get())?;
                }
                f.write_str(") and still fails, so stepping back its loaded cores is not helping")
            }
        }
    }
}

/// The voltage-targeted backoffs of one load since its latest passing trial. A failure that backed
/// off one core on each CCD counts once. A pending failure keeps the snapshot of its load as it
/// folded.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Backoffs(Vec<Strike>);

impl Backoffs {
    /// The failures backed off.
    pub(crate) fn failures(&self) -> u32 {
        let mut seen: Vec<Seq> = Vec::new();
        for strike in &self.0 {
            if !seen.contains(&strike.failure) {
                seen.push(strike.failure);
            }
        }
        u32::try_from(seen.len()).unwrap_or(u32::MAX)
    }

    pub(crate) fn decisions(&self) -> Vec<Seq> {
        self.0.iter().map(|strike| strike.decision).collect()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Drops the strikes whose failure `discarded` reports.
    pub(crate) fn forget(&mut self, discarded: impl Fn(Seq) -> bool) {
        self.0.retain(|strike| !discarded(strike.failure));
    }

    /// Whether a live unattributed failure of `load` at `profile`, affecting `affected` CCDs, is
    /// located instead of backed off: an unloaded core is off CO 0, and the load already had
    /// [`ESCALATE_AFTER`] backoffs or no loaded core on the affected CCDs can step back. The
    /// caller has already decided the failure is live, unattributed in routing, and would
    /// otherwise need a response.
    pub(crate) fn escalation(
        &self,
        topology: &Topology,
        load: LoadKey,
        profile: &Profile,
        affected: &[CcdId],
    ) -> Option<Escalation> {
        if !unloaded_off_zero(topology, load, profile) {
            return None;
        }
        if loaded_stuck_at_zero(topology, load, profile, affected) {
            return Some(Escalation::LoadedAtZero);
        }
        let failures = self.failures();
        (failures >= ESCALATE_AFTER).then(|| Escalation::Backoffs {
            failures,
            decisions: self.decisions(),
        })
    }
}

/// Per load, the voltage-targeted backoffs since the load's latest passing trial.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Strikes {
    loads: HashMap<LoadKey, Backoffs>,
}

impl Strikes {
    /// Counts the backoff `decision`, an answer to `failure` of `load`, when its kind counts.
    pub(crate) fn backoff(
        &mut self,
        load: LoadKey,
        kind: BackoffKind,
        decision: Seq,
        failure: Seq,
    ) {
        if kind.counts() {
            self.loads
                .entry(load)
                .or_default()
                .0
                .push(Strike { decision, failure });
        }
    }

    /// A passing trial of exactly this load, live or carried, whatever its duration, restarts
    /// its count. A pass of any other load does not.
    pub(crate) fn pass(&mut self, load: LoadKey) {
        self.loads.remove(&load);
    }

    /// Drops the strikes a reset of `core` discards: every strike of a load that includes it, and
    /// any other strike whose failure `named` reports as naming it.
    pub(crate) fn reset_core(&mut self, core: CoreId, named: impl Fn(Seq) -> Option<CoreId>) {
        self.loads.retain(|load, backoffs| {
            if load.loaded.contains(core) {
                return false;
            }
            backoffs.forget(|failure| named(failure) == Some(core));
            !backoffs.is_empty()
        });
    }

    /// The backoffs of `load` since its latest pass.
    pub(crate) fn snapshot(&self, load: LoadKey) -> Backoffs {
        self.loads.get(&load).cloned().unwrap_or_default()
    }

    /// The failures of `load` backed off since its latest pass.
    pub(crate) fn failures(&self, load: LoadKey) -> u32 {
        self.loads.get(&load).map_or(0, Backoffs::failures)
    }

    /// Which backoff of its load the next one is, since the last pass.
    pub(crate) fn next_ordinal(&self, load: LoadKey) -> u32 {
        self.failures(load) + 1
    }

    pub(crate) fn escalation(
        &self,
        topology: &Topology,
        load: LoadKey,
        profile: &Profile,
        affected: &[CcdId],
    ) -> Option<Escalation> {
        self.snapshot(load)
            .escalation(topology, load, profile, affected)
    }
}

/// Whether some core outside `load` is off CO 0 in `profile`.
pub(crate) fn unloaded_off_zero(topology: &Topology, load: LoadKey, profile: &Profile) -> bool {
    topology
        .all()
        .iter()
        .any(|core| !load.loaded.contains(core) && profile.at(core).signed() != 0)
}

/// Whether no loaded core on the `affected` CCDs is off CO 0 in `profile`: backing off the loaded
/// cores has nothing to move.
pub(crate) fn loaded_stuck_at_zero(
    topology: &Topology,
    load: LoadKey,
    profile: &Profile,
    affected: &[CcdId],
) -> bool {
    !load.loaded.iter().any(|core| {
        topology
            .ccd_of(core)
            .is_some_and(|ccd| affected.contains(&ccd))
            && profile.at(core).signed() != 0
    })
}
