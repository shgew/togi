#![expect(
    clippy::unwrap_used,
    reason = "the tests run the command against memory and panic on a broken fixture"
)]

mod commands;
mod events;
#[path = "../support/foreign.rs"]
mod fixture;
mod foreign;
use crate::support as host;
mod status;
mod support;
mod watch;
