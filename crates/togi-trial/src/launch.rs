use std::ffi::OsString;
use std::path::PathBuf;

use togi_backend::Plan;
use togi_machine::CpuId;

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
    /// The CPUs the launcher itself is confined to from its first instruction; used only without
    /// a scope, where nothing else confines it.
    pub(crate) pin: Vec<CpuId>,
}

pub(crate) fn cpus_list(cpus: &[CpuId]) -> String {
    cpus.iter()
        .map(|cpu| cpu.0.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// `systemd-run [--user] --scope --quiet --collect --unit <scope> --uid <uid> --gid <gid>
/// --working-directory <dir> -p AllowedCPUs=<cpus> -p DefaultDependencies=no -- <argv>`.
///
/// `DefaultDependencies=no` keeps a system shutdown from stopping the scope on its own.
pub(crate) fn scope_argv(
    unit: &str,
    identity: BackendIdentity,
    user_manager: bool,
    cwd: &std::path::Path,
    cpus: &[CpuId],
    program: &[OsString],
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec!["systemd-run".into()];
    if user_manager {
        argv.push("--user".into());
    }
    argv.extend(
        [
            "--scope".to_owned(),
            "--quiet".to_owned(),
            "--collect".to_owned(),
            "--unit".to_owned(),
            unit.to_owned(),
            "--uid".to_owned(),
            identity.uid().to_string(),
            "--gid".to_owned(),
            identity.gid().to_string(),
            "--working-directory".to_owned(),
        ]
        .map(OsString::from),
    );
    argv.push(cwd.as_os_str().to_owned());
    argv.extend(
        [
            "-p".to_owned(),
            format!("AllowedCPUs={}", cpus_list(cpus)),
            "-p".to_owned(),
            "DefaultDependencies=no".to_owned(),
            "--".to_owned(),
        ]
        .map(OsString::from),
    );
    argv.extend_from_slice(program);
    argv
}

/// The command that starts `plan` in `cwd`: through a confining scope, or bare when unscoped.
pub(crate) fn command_for(
    confinement: Confinement,
    scope: &ScopeName,
    cwd: &std::path::Path,
    cpus: &[CpuId],
    plan: &Plan,
) -> LaunchCommand {
    let mut program: Vec<OsString> = vec![plan.program.as_os_str().to_owned()];
    program.extend(plan.args.iter().cloned());
    let argv = match confinement {
        Confinement::Scoped {
            identity,
            user_manager,
        } => scope_argv(scope.as_str(), identity, user_manager, cwd, cpus, &program),
        Confinement::Unscoped => program,
    };
    LaunchCommand {
        argv,
        cwd: cwd.to_owned(),
        pin: match confinement {
            Confinement::Scoped { .. } => Vec::new(),
            Confinement::Unscoped => cpus.to_vec(),
        },
    }
}

/// Spawns a launcher with its own process group, groups cleared as root and a parent-death
/// SIGKILL. Only the runtime thread calls this: the death signal fires when the forking thread
/// dies, so a launch from a short-lived thread would kill the workload with it.
#[cfg(target_os = "linux")]
#[expect(
    unsafe_code,
    reason = "pre_exec runs between fork and exec and has no safe equivalent"
)]
pub(crate) fn spawn(command: &LaunchCommand) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    use rustix::io::Errno;
    use rustix::process::{Signal, getpid, getppid, set_parent_process_death_signal};

    let (program, args) = command
        .argv
        .split_first()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let root = rustix::process::geteuid().is_root();
    let parent = getpid();
    let mut pinned = rustix::thread::CpuSet::new();
    for cpu in &command.pin {
        pinned.set(usize::from(cpu.0));
    }
    let pin = !command.pin.is_empty();
    let mut launcher = Command::new(program);
    launcher
        .args(args)
        .current_dir(&command.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // SAFETY: the closure only makes raw syscalls (setgroups, prctl, getppid) and builds io errors
    // from errno values, so it neither allocates nor takes locks between fork and exec.
    unsafe {
        launcher.pre_exec(move || {
            if root {
                nix::unistd::setgroups(&[]).map_err(std::io::Error::from)?;
            }
            if pin {
                rustix::thread::sched_setaffinity(None, &pinned)?;
            }
            set_parent_process_death_signal(Some(Signal::KILL))?;
            if getppid() != Some(parent) {
                return Err(Errno::SRCH.into());
            }
            Ok(())
        });
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the one spawn of the workload launcher: pre_exec needs std's Command"
    )]
    launcher.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> ScopeName {
        ScopeName::from_unit("togi-trial-t1-c00.scope").unwrap()
    }

    fn strings(argv: &[OsString]) -> Vec<String> {
        argv.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn the_scope_argv_is_exactly_the_documented_command() {
        let identity = BackendIdentity::new(997, 996).unwrap();
        let argv = scope_argv(
            scope().as_str(),
            identity,
            false,
            std::path::Path::new("/trials/t1/c00"),
            &[CpuId(2), CpuId(18)],
            &["/bin/mprime".into(), "-t".into()],
        );
        assert_eq!(
            strings(&argv),
            [
                "systemd-run", "--scope", "--quiet", "--collect", "--unit", "togi-trial-t1-c00",
                "--uid", "997", "--gid", "996", "--working-directory", "/trials/t1/c00", "-p",
                "AllowedCPUs=2,18", "-p", "DefaultDependencies=no", "--", "/bin/mprime", "-t",
            ]
        );
    }

    #[test]
    fn a_user_manager_adds_the_user_flag_first() {
        let identity = BackendIdentity::new(1001, 1002).unwrap();
        let argv = scope_argv(scope().as_str(), identity, true, std::path::Path::new("/"), &[CpuId(0)], &["x".into()]);
        assert_eq!(strings(&argv)[..2], ["systemd-run", "--user"]);
    }

    #[test]
    fn unscoped_commands_run_the_program_directly() {
        let plan = Plan {
            program: "/bin/true".into(),
            args: vec!["a".into()],
            inputs: Vec::new(),
            watched: Vec::new(),
        };
        let scoped = command_for(
            Confinement::Scoped {
                identity: BackendIdentity::new(997, 996).unwrap(),
                user_manager: false,
            },
            &scope(),
            std::path::Path::new("/tmp"),
            &[CpuId(0)],
            &plan,
        );
        assert!(scoped.pin.is_empty(), "the scope's cpuset confines it");
        let command = command_for(Confinement::Unscoped, &scope(), std::path::Path::new("/tmp"), &[CpuId(0)], &plan);
        assert_eq!(strings(&command.argv), ["/bin/true", "a"]);
        assert_eq!(command.cwd, std::path::Path::new("/tmp"));
        assert_eq!(command.pin, [CpuId(0)]);
    }
}
