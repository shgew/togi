//! Public-API tests of the night policy: coverage matrix, edge U and mapping, scheduler.

#![expect(
    clippy::unwrap_used,
    reason = "integration test fixtures and assertions panic on invalid evidence"
)]

mod edge;
mod exposure;
mod fixture;
mod matrix;
mod schedule;
