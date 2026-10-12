//! Forecasts of a real run from a copy of its state directory. `bench --forecast` writes them and `stats
//! --forecast` scores them, both through this crate.
//!
//! A forecast starts after an [`Anchor`], the last event of the copied live journal. Each simulated run of the
//! ensemble resumes a copy of the directory on a simulated machine and ends in an [`Outcome`]; [`Record::new`]
//! summarizes the runs and [`Record::write`] commits them. After the real run, [`read_dir`] reads the directory
//! again, and [`Record::score`] measures what follows the anchor the way the forecast measured its runs and
//! places each metric against the ensemble's range.
//!
//! The crate reads journals and never writes a state directory, takes its lock or runs a simulation: the
//! simulated runs, their seeds and their order belong to the caller, and every function here is a pure function
//! of its inputs, so a record is the same however the runs were scheduled.
//!
//! ```
//! use togi_machine::{CoreId, ObservedOffset, PerCore};
//! use togi_tool_forecast::{MetricKind, Outcome, Run, Status, metrics, summarize};
//!
//! let core = CoreId::new(0)?;
//! let run = |hours, offset| Run {
//!     machine: "m.json".to_owned(),
//!     seed: 1,
//!     split: "dev".to_owned(),
//!     outcome: Outcome {
//!         status: Status::Concluded,
//!         hours,
//!         crashes: 0,
//!         hunts: 0,
//!         profile: [(core, ObservedOffset(offset))].into_iter().collect::<PerCore<_>>(),
//!         depth: offset,
//!     },
//! };
//! let runs = [run(2.0, -10), run(3.0, -20), run(5.0, -30)];
//! assert_eq!(summarize(&runs)?.hours.map(|hours| hours.median), Some(3.0));
//! let all = metrics(&runs)?;
//! assert_eq!(all[0].kind, MetricKind::Core(core));
//! assert_eq!(all[0].percentile(-20.0, false), Some(50.0));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod error;
mod outcome;
mod record;
mod score;
mod session;
mod summary;

#[cfg(test)]
mod tests;

pub use error::Error;
pub use outcome::{Outcome, Status, after, recovery_bias};
pub use record::{File, Record, Run};
pub use score::{Flag, Placement, Row, Score};
pub use session::{Anchor, Session, read_dir, read_dir_lenient};
pub use summary::{Metric, MetricKind, Range, Summary, metrics, summarize};
