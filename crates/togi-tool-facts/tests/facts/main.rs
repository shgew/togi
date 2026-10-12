#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests build a journal in memory, dump it to disk, run the built command and panic on a broken fixture"
)]

mod cli;
mod fixture;
mod generate;
