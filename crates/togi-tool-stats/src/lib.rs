//! `togi-tool-stats`: the development program that summarizes a current or archived journal for review
//! (`docs/reviewing.md`), or scores a real run against a forecast the `bench --forecast` wrote.
//!
//! [`Journal::read`] reads a journal whole without taking its lock; [`report`] renders it as plain text
//! tables, sorted and deterministic for a fixed journal. [`score`] reads a state directory's journals from a
//! forecast's anchor onward and renders how the run compares with the forecast's ensemble.
//!
//! ```
//! use togi_tool_stats::Journal;
//!
//! let empty = Journal::new(Vec::new());
//! assert!(empty.is_err());
//! ```

mod error;
mod evidence;
mod forecast;
mod hunts;
mod journal;
mod project;
mod r7;
mod report;
mod table;

#[cfg(test)]
mod tests;

pub use error::Error;
pub use forecast::{Scored, score};
pub use journal::Journal;
pub use report::report;
