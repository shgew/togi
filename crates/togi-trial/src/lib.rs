//! Runs stress-program trials in confined systemd scopes and proves every one stopped.
//!
//! One runtime thread owns every launch, one supervisor task owns each trial's state, and the
//! synchronous seams of `togi_machine` talk to them over channels. Teardown is a single typestate
//! pipeline under one absolute budget; only its final state mints `Stopped`.

mod error;
mod evidence;
mod files;
mod id;
mod identity;
mod instance;
mod judge;
mod launch;
mod os;
mod output;
mod plan;
mod procfs;
mod runner;
mod runtime;
mod samples;
mod schedule;
mod sensors;
mod stall;
mod supervisor;
mod sweep;
mod teardown;
mod timing;
#[cfg(test)]
mod testing;
mod watch;

pub use error::{CleanupFailure, Containment, Error, OutputFailure};
pub use files::PrepareError;
pub use identity::{BackendIdentity, IdentityError};
pub use plan::InvalidTrial;
pub use runner::{Backends, Options, Runner, Trial};
pub use samples::{StorageError, StorageFailure};
