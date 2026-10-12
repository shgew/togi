#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests run the built command, read the state directories it leaves and panic on a broken fixture"
)]

mod common;
mod inputs;
mod resume;
mod session;
mod suite;
