use std::num::NonZeroU32;
use togi_journal::{Build, Event, FixStamp, SCHEMA};
use togi_sim::Simulator;
use togi_simrun::Persistence;
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use crate::Error;

/// The build every simulated journal names, whatever release this crate is compiled into: version `0.0.0`, no
/// revision, and the tuner's current ruleset and evidence epoch.
pub fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: EVIDENCE_EPOCH,
    }
}

/// Run a scripted night through the real session and end it by signal at the selected event, or
/// let it reach its own dead end. The event and boot limits fail the fixture if the intended
/// path regresses rather than silently returning a much longer journal.
pub(super) fn simulate(
    machine: Simulator,
    persistence: &Persistence,
    max_boots: NonZeroU32,
    halt: impl FnMut(&Event) -> bool + 'static,
) -> Result<Vec<Event>, Error> {
    Ok(togi_simrun::scripted::night(
        machine,
        persistence,
        &build(),
        max_boots,
        halt,
    )?)
}
