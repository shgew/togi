//! The `togi watch` dashboard.
//!
//! [`model`] folds the journal into facts and lends a read-only [`model::Snapshot`] of them, [`history`] keeps
//! what happened as structured entries, [`words`] is the one table that turns each kind of event into words, and
//! [`screen`] is the seam between the frames, which draw one snapshot, and the terminal loop, which decides when
//! to draw. [`frames`] draws a frame: the main view, help and the journal view.
//!
//! Display coordinates and history ordinals saturate at their rendering limits. Elapsed displays
//! clamp backwards wall-clock corrections to zero while retaining representable positive spans.

#[cfg(test)]
mod corpus;
pub mod day;
mod derive;
mod describe;
mod facts;
pub mod frames;
pub mod history;
pub mod model;
mod numbers;
pub mod screen;
pub mod terminal;
pub mod tuning;
pub mod words;

#[cfg(test)]
mod tests;
