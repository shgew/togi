#![expect(
    clippy::unwrap_used,
    reason = "the tests panic on a broken fixture and run the built command"
)]

#[path = "../support/foreign.rs"]
#[expect(
    dead_code,
    reason = "disk fixture adapters are used only by read-side PTY tests"
)]
mod fixture;
mod foreign;
mod lifecycle;
mod panics;
mod pty;
mod support;
