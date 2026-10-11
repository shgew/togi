//! A deterministic simulated Zen 5 machine.
//!
//! This part is the pure model: the machine-file document ([`definition`]), the validated [`Model`] that
//! resolves it, the hazard model ([`hazard`], [`voltage`]), seeded randomness ([`random`]), machine files
//! on disk ([`file`]) and replay facts ([`replay`]). It holds no clock, registers or trial state.

mod coreset;
pub mod definition;
mod error;
pub mod file;
pub mod hazard;
pub mod model;
pub mod random;
pub mod replay;
mod vocab;
pub mod voltage;

pub use coreset::CoreSet;
pub use definition::MachineDefinition;
pub use error::{InvalidMachine, Problem, ShapeError};
pub use hazard::{HazardSource, Load, SourceKind};
pub use model::Model;
pub use vocab::{RegimeId, Reset, ResetMix, SignalKind, SignalMix};
