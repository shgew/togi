#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests build extracts, run the built command and panic on a broken fixture"
)]

mod cli;
mod determinism;
mod support;
