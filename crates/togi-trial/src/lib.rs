//! Runs stress-program trials in confined systemd scopes and proves every one stopped.
//!
//! One runtime thread owns every launch, one supervisor task owns each trial's state, and the
//! synchronous seams of `togi_machine` talk to them over channels. Teardown is a single typestate
//! pipeline under one absolute budget; only its final state mints `Stopped`.

mod error;
mod identity;
mod runner;

#[cfg(target_os = "linux")]
mod evidence;
#[cfg(target_os = "linux")]
mod files;
#[cfg(target_os = "linux")]
mod id;
#[cfg(target_os = "linux")]
mod instance;
#[cfg(target_os = "linux")]
mod join;
#[cfg(target_os = "linux")]
mod judge;
#[cfg(target_os = "linux")]
mod launch;
#[cfg(target_os = "linux")]
mod logs;
#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
mod os;
#[cfg(target_os = "linux")]
mod output;
#[cfg(target_os = "linux")]
mod plan;
#[cfg(target_os = "linux")]
mod prepare;
#[cfg(target_os = "linux")]
mod procfs;
#[cfg(target_os = "linux")]
mod runtime;
#[cfg(target_os = "linux")]
mod samples;
#[cfg(target_os = "linux")]
mod schedule;
#[cfg(target_os = "linux")]
mod sensors;
#[cfg(target_os = "linux")]
mod stall;
#[cfg(target_os = "linux")]
mod supervisor;
#[cfg(target_os = "linux")]
mod sweep;
#[cfg(target_os = "linux")]
mod teardown;
#[cfg(test)]
#[cfg(target_os = "linux")]
mod testing;
#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
#[cfg(target_os = "linux")]
mod timing;
#[cfg(not(target_os = "linux"))]
mod unsupported;
#[cfg(target_os = "linux")]
mod watch;

pub use error::{
    CleanupFailure, Containment, Error, InvalidId, InvalidTrial, Joined, OutputFailure,
    PrepareError, StorageError, StorageFailure,
};
pub use identity::{BackendIdentity, IdentityError};
pub use runner::{Backends, Options, Runner, Trial};
