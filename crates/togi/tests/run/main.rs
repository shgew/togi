#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests panic on a broken fixture and run the built command"
)]

#[path = "../support/foreign.rs"]
mod fixture;
mod foreign;
mod lifecycle;
mod panics;
mod pty;
mod support;
