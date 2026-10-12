//! The `togi watch` dashboard.
//!
//! [`model`] folds the journal into facts and lends a read-only [`model::Snapshot`] of them, [`history`] keeps
//! what happened as structured entries, [`words`] is the one table that turns each kind of event into words, and
//! [`screen`] is the seam between the frames, which draw one snapshot, and the terminal loop, which decides when
//! to draw.

mod derive;
mod describe;
mod facts;
pub mod history;
pub mod model;
pub mod screen;
pub mod words;

#[cfg(test)]
mod tests;
