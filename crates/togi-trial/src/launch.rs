use std::ffi::OsString;
use std::path::PathBuf;

use crate::id::ScopeName;
use crate::identity::BackendIdentity;

/// How instances are confined. `Unscoped` exists for the integration tests' real helpers only and
/// is never containment: nothing then proves a descendant stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Confinement {
    Scoped {
        identity: BackendIdentity,
        user_manager: bool,
    },
    Unscoped,
}

/// A program to start: a plain argv in a working directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaunchCommand {
    pub(crate) argv: Vec<OsString>,
    pub(crate) cwd: PathBuf,
}

/// `systemd-run [--user] --scope --quiet --collect --unit <scope> --uid <uid> --gid <gid>
/// --working-directory <dir> -p AllowedCPUs=<cpus> -p DefaultDependencies=no -- <argv>`.
pub(crate) fn scope_argv(
    scope: &ScopeName,
    confinement: Confinement,
    cwd: &std::path::Path,
    cpus: &[togi_machine::CpuId],
    program: &[OsString],
) -> Vec<OsString> {
    let _ = (scope, confinement, cwd, cpus, program);
    unimplemented!()
}

/// Spawns a launcher with its own process group, groups cleared as root and parent-death SIGKILL;
/// only the runtime thread calls this, because the death signal fires when the forking thread dies.
#[cfg(target_os = "linux")]
pub(crate) fn spawn(command: &LaunchCommand) -> std::io::Result<std::process::Child> {
    let _ = command;
    unimplemented!()
}
