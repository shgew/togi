//! Compare exact or retained journal semantics across trees on a fixed set of scripted sessions.

pub mod cache;
mod error;
mod pool;
mod process;
pub mod same;
mod simulator;

pub use error::{BuildFailure, Error, RunError, Unrunnable};
pub use process::Fleet;

#[cfg(test)]
mod replay_tests;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
