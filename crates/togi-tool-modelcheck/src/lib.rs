//! The model check: a simulated machine judged against the real trial facts, group by group.
//!
//! Real trials of a machine are grouped by BIOS context, trial class and depth (the shallowest offset
//! over the loaded cores). For each group of at least [`MIN_GROUP_TRIALS`] trials, the simulated machine
//! predicts every trial's failure probability over its intended duration; the mean of those is the
//! chance that one trial of the group fails. The machine passes when the observed failure count lies in
//! the central 99% [`Interval`] of `Binomial(n, mean)`, and a group outside it is *flagged*. Failures
//! outside any trial have no exposure to predict over and are only counted.
//!
//! Two kinds of caller use it:
//!
//! * A report: [`check`] loads a machine's own extract and returns a [`Check`] to print with [`report`]
//!   or serialize; [`check_records`] does the same for records the caller already holds.
//! * A search: build a [`Checker`] once with [`Checker::new`], then ask [`Checker::accepts`] of each
//!   candidate [`Model`] (an exact test with no interval work) or [`Checker::groups`] for the flagged
//!   groups a constraint should respect.
//!
//! A machine whose model gives a `thermal_trip` or `power_loss` reset any weight is refused: those resets
//! can turn a simulated crash into an inconclusive trial, so its failure probability is not comparable
//! with decisive-only counts.
//!
//! ```
//! use togi_sim::{MachineDefinition, Model};
//! use togi_tool_modelcheck::Checker;
//!
//! let model = Model::new(&MachineDefinition::default(), 0)?;
//! let checker = Checker::new(&model, &[])?;
//! assert!(checker.accepts(&model)?);
//! assert_eq!(checker.idle_failures(), 0);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod checker;
mod error;
mod interval;
mod report;

use std::path::Path;

use togi_sim::{MachineDefinition, Model};
use togi_tool_trialfacts::{Extracts, TrialFact};

pub use checker::{Checker, Group, MIN_GROUP_TRIALS};
pub use error::{Error, FactProblem};
pub use interval::Interval;
pub use report::{Check, Status, report};

/// Checks the machine file at `machine_path`, defined by `definition`, against the extract its `facts`
/// names. The extract is read through `extracts`, so a machine shared between callers is read once.
///
/// The machine is built with seed 0, which matters only to a machine with no `core` limits to draw from.
pub fn check(
    machine_path: &Path,
    definition: &MachineDefinition,
    extracts: &mut Extracts,
) -> Result<Check, Error> {
    let model = Model::new(definition, 0)?;
    let (extract, records) = extracts.load(machine_path, definition)?;
    check_records(machine_path, &extract, &model, &records)
}

/// [`check`] for records the caller already read; `machine` and `extract` only label the result.
pub fn check_records(
    machine: &Path,
    extract: &Path,
    model: &Model,
    records: &[TrialFact],
) -> Result<Check, Error> {
    let checker = Checker::new(model, records)?;
    Ok(checker.check(machine, extract, model)?)
}
