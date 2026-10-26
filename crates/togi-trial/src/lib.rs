//! Runs stress-program trials in confined systemd scopes and proves every one stopped.
//!
//! One runtime thread owns every launch, one supervisor task owns each trial's state, and the
//! synchronous seams of `togi_machine` talk to them over channels. Teardown is a single typestate
//! pipeline under one absolute budget; only its final state mints `Stopped`.

mod error;
mod identity;
mod runner;

#[cfg(target_os = "linux")]
mod clock;
#[cfg(target_os = "linux")]
mod content;
#[cfg(target_os = "linux")]
mod controller;
#[cfg(any(test, target_os = "linux"))]
mod evidence;
#[cfg(target_os = "linux")]
mod files;
#[cfg(target_os = "linux")]
mod frequency;
#[cfg(any(test, target_os = "linux"))]
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        dead_code,
        reason = "off Linux only the fixture tests compile; the Linux runner uses the rest"
    )
)]
mod id;
#[cfg(target_os = "linux")]
mod instance;
#[cfg(target_os = "linux")]
mod join;
#[cfg(target_os = "linux")]
mod judge;
#[cfg(any(test, target_os = "linux"))]
mod launch;
#[cfg(target_os = "linux")]
mod logs;
#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
mod os;
#[cfg(any(test, target_os = "linux"))]
mod output;
#[cfg(any(test, target_os = "linux"))]
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        dead_code,
        reason = "off Linux only the fixture tests compile; the Linux runner uses the rest"
    )
)]
mod plan;
#[cfg(target_os = "linux")]
mod prepare;
#[cfg(any(test, target_os = "linux"))]
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        dead_code,
        reason = "off Linux only the fixture tests compile; the Linux runner uses the rest"
    )
)]
mod procfs;
#[cfg(target_os = "linux")]
mod runtime;
#[cfg(target_os = "linux")]
mod samples;
#[cfg(any(test, target_os = "linux"))]
mod schedule;
#[cfg(any(test, target_os = "linux"))]
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        dead_code,
        reason = "off Linux only the fixture tests compile; the Linux runner uses the rest"
    )
)]
mod sensors;
#[cfg(any(test, target_os = "linux"))]
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
#[cfg(any(test, target_os = "linux"))]
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        dead_code,
        reason = "off Linux only the fixture tests compile; the Linux runner uses the rest"
    )
)]
mod timing;
#[cfg(not(target_os = "linux"))]
mod unsupported;
#[cfg(target_os = "linux")]
mod watch;

pub use error::{
    CleanupFailure, Containment, Error, InvalidId, InvalidTrial, Joined, OutputFailure,
    PrepareError, StorageError, StorageFailure, SystemdError,
};
#[cfg(target_os = "linux")]
pub use frequency::{Error as FrequencyError, Sysfs as CpuFrequencySysfs};
pub use identity::{BackendIdentity, IdentityError};
pub use runner::{Backends, Options, Runner, Trial};
/// The roles a trial's logical CPUs play, so a caller can build and read a `TrialSpec` through
/// this crate alone.
pub use togi_machine::{InvalidRoles, PhaseSchedule, TrialRole};
