//! `togi-tool-bench`: runs the suite of simulated sessions on the simulator command, measures each run's
//! journal, and compares a run with a recorded baseline. It is a development tool and never ships.
//!
//! A run goes through five steps, each its own module:
//!
//! 1. [`Suite`] reads `suite.json` and plans one [`RunSpec`] per seed of the selected [`Selection`], with each
//!    machine file and facts extract read once.
//! 2. [`Simulator`] runs each session as a subprocess with a state directory of its own, up to `--jobs` at a
//!    time, and reads back its journal and log.
//! 3. [`measure`] turns the journal into the [`Measured`] numbers of a [`Record`]: time, crashes, trials, real
//!    answers, the final profile and the model's hazards at it.
//! 4. [`summary::report`] and [`compare::report`] print the per-scenario summary and the comparison with a
//!    baseline, and [`sanity::compare`] checks a run against the scores of a recorded baseline.
//! 5. [`cache`] keeps each finished record under the digests of the simulator, the bench, the flags and every
//!    input file, so an unchanged run is not simulated again.
//!
//! Records, the summary, the comparison and the cache are this crate's own formats: JSON Lines of [`Record`],
//! plain text reports, and one JSON file per cached run. The gate of a ruleset is the `gate` object of the suite
//! file, typed and judged by [`gate`].
//!
//! Two modes do not measure the suite. [`same`] proves two trees decide the same on every simulated session, or
//! only the suite's smoke sessions, by comparing their journals. [`forecast`] resumes copies of a real run's
//! state directory on the simulator with the suite's `target` scenario and summarizes what the runs did.
//!
//! ```
//! use togi_tool_bench::{Selection, Suite};
//!
//! let suite = Suite::parse(
//!     br#"{"scenarios":[{"name":"default","dev":[1,2],"holdout":[101]}]}"#,
//!     ".".into(),
//! )?;
//! let plan = suite.plan(Selection::Dev)?;
//! let names: Vec<_> = plan.runs.iter().map(|run| run.name()).collect();
//! assert_eq!(names, ["default/dev-1", "default/dev-2"]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#[cfg(test)]
extern crate self as togi_tool_bench;

pub mod cache;
pub mod compare;
mod error;
pub mod forecast;
pub mod gate;
mod git;
mod metrics;
mod pool;
mod process;
mod record;
mod run;
pub mod same;
pub mod sanity;
mod simulator;
mod stats;
mod suite;
pub mod summary;

pub use error::{
    BuildFailure, Error, GitError, MeasureError, ModelCheckError, RunError, Unrunnable,
};
pub use git::{Tree, inspect};
pub use metrics::{Measured, measure};
pub use process::Fleet;
pub use record::{
    ModelCheck, Record, Status, encode as encode_records,
    encode_baseline as encode_baseline_records, read as read_records,
    read_from as read_records_from, write as write_records,
    write_baseline as write_baseline_records,
};
pub use run::{Options, Report, execute, set_scenario_shares};
pub use simulator::{Session, Simulator};
pub use suite::{Machine, Plan, RunSpec, Selection, Split, Suite, SuiteError, SuiteSource};
