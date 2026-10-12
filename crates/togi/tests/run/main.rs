#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests panic on a broken fixture and run the built command"
)]

mod lifecycle;
mod panics;
mod pty;
mod support;
