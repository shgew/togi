#![expect(
    clippy::expect_used,
    reason = "helpers and proptest strategies panic on a broken fixture"
)]

mod ccd;
mod common;
mod definition;
mod encode;
mod hazard;
mod machines;
mod overlay;
mod probability;
mod random;
mod replay;
mod shared_voltage;
mod validation;
