//! `togi-tool-stats`: the development program that summarizes a current or archived journal for review
//! (`docs/reviewing.md`).
//!
//! [`Journal::read`] reads a journal whole without taking its lock; [`report`] renders it as plain text
//! tables, sorted and deterministic for a fixed journal.
//!
//! ```
//! use togi_tool_stats::Journal;
//!
//! let empty = Journal::new(Vec::new());
//! assert!(empty.is_err());
//! ```

mod depth;
mod error;
mod journal;
mod project;
mod report;
mod requests;
mod table;

#[cfg(test)]
mod tests;

pub use error::Error;
pub use journal::Journal;
pub use report::report;
