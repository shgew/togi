//! A deterministic simulated Zen 5 machine.
//!
//! A [`Simulator`] owns virtual clocks, boots, registers, kernel logs and armed [`faults`].
//! It exposes the [`togi_machine::Machine`] seams and takes its trial outcomes from a
//! [`ScriptedMachine`].

mod adapters;
mod content;
mod cores;
pub mod faults;
mod persisted;
mod random;
mod runtime;
mod samples;
mod simulator;
mod source;
mod trials;

pub use content::content_id;
pub use persisted::{Persisted, RestoreError};
pub use runtime::{ArithmeticError, EPOCH, History, REBOOT_TIME};
pub use samples::SampleStorage;
pub use simulator::Simulator;
pub use source::{
    DEFAULT_BIOS_VERSION, DEFAULT_BOARD, DEFAULT_BOOST_LIMIT_MHZ, DEFAULT_MICROCODE,
    ReportedFailure, ScriptedMachine, ScriptedMce, ScriptedTopology, ScriptedVoltageSamples,
    TrialOutcome,
};
pub use trials::CONTROLLER_PID;
