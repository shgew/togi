#![expect(
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "helpers and proptest strategies panic on a broken fixture, and the tests drive the SMU directly"
)]

mod ccd;
mod common;
mod definition;
mod draws;
mod encode;
mod hazard;
mod hooks;
mod machines;
mod overlay;
mod probability;
mod random;
mod replay;
mod runtime;
mod samples;
mod seams;
mod shared_voltage;
mod trials;
mod validation;
