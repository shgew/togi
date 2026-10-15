#![expect(
    clippy::unwrap_used,
    reason = "the tests build extracts, run the built command and panic on a broken fixture"
)]

use togi_tool_fit::{Error, MachineOutput};

mod cli;
mod determinism;
#[expect(
    dead_code,
    reason = "the shared memory fixture also serves unit-test output assertions"
)]
mod output;
mod support;
