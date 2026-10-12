#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests run the built command and panic on a broken fixture"
)]

mod commands;
mod events;
#[path = "../support/foreign.rs"]
mod fixture;
mod foreign;
#[path = "../support/mod.rs"]
mod host;
mod status;
mod support;
mod watch;
