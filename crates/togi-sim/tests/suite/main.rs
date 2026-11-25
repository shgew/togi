#![expect(
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "helpers panic on a broken fixture, and the tests drive the SMU directly"
)]

mod common;
mod hooks;
mod runtime;
mod samples;
mod scripted;
mod seams;
mod trials;
