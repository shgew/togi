#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests simulate sessions in memory, write their journals to disk, run the built command and panic on a broken fixture"
)]

mod cli;
mod sim;
mod simulated;
