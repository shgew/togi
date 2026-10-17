//! A deterministic simulated Zen 5 machine.
//!
//! The pure model: the machine-file document ([`definition`]), the validated [`Model`] that
//! resolves it, the hazard model ([`hazard`], [`voltage`]), seeded randomness ([`random`]), machine files
//! on disk ([`mod@file`]) and replay facts ([`replay`]).
//!
//! The runtime: a [`Simulator`] owns the virtual clocks, boots, registers, kernel logs and armed
//! [`faults`] of one machine, and hands out its seams as a [`togi_machine::Machine`].

mod adapters;
mod cores;
pub mod definition;
mod error;
pub mod faults;
pub mod file;
pub mod hazard;
pub mod model;
mod persisted;
pub mod random;
pub mod replay;
mod runtime;
mod samples;
mod simulator;
mod trials;
mod vocab;
pub mod voltage;

pub use definition::MachineDefinition;
pub use error::{InvalidMachine, Problem, ShapeError};
pub use hazard::{HazardSource, Load, SourceKind, saturating_count};
pub use model::Model;
pub use persisted::{Persisted, RestoreError};
pub use runtime::{History, REBOOT_TIME, RunSettings};
pub use samples::SampleStorage;
pub use simulator::{ScriptError, Simulator};
pub use vocab::{RESETS, ResetMix, SignalMix, WEIGHTED_SIGNALS, regime_index};
