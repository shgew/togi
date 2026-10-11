//! A deterministic simulated Zen 5 machine.
//!
//! The pure model: the machine-file document ([`definition`]), the validated [`Model`] that
//! resolves it, the hazard model ([`hazard`], [`voltage`]), seeded randomness ([`random`]), machine files
//! on disk ([`file`]) and replay facts ([`replay`]).
//!
//! The runtime: a [`Simulator`] owns the virtual clocks, boots, registers, kernel logs and armed
//! [`faults`] of one machine, and hands out its seams as a [`togi_machine::Machine`].

mod cores;
pub mod definition;
mod error;
pub mod file;
pub mod hazard;
pub mod model;
pub mod random;
pub mod replay;
mod vocab;
pub mod voltage;

pub use definition::MachineDefinition;
pub use error::{InvalidMachine, Problem, ShapeError};
pub use hazard::{HazardSource, Load, SourceKind};
pub use model::Model;
pub use vocab::{RESETS, ResetMix, SignalMix, WEIGHTED_SIGNALS, regime_index};
