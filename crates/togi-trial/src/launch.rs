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
    /// The CPUs the launcher itself is confined to from its first instruction. A forked launcher
    /// inherits the affinity of the thread that forks it, which is togi's controller set while a
    /// trial runs, so the launcher must be given the workload's CPUs and never the controller's;
    /// a scope's cpuset then narrows it no further.
    pub(crate) pin: Vec<CpuId>,
}

pub(crate) fn cpus_list(cpus: &[CpuId]) -> String {
    cpus.iter()
        .map(|cpu| cpu.get().to_string())
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
        pin: cpus.to_vec(),
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
        pinned.set(usize::from(cpu.get()));
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
    // SAFETY: between fork and exec the child has one thread, and the closure only makes rustix's
    // direct syscalls (setgroups, sched_setaffinity, prctl, getppid; never glibc's setxid
    // broadcast, which takes locks) and turns errno values into io errors without allocating.
    unsafe {
        launcher.pre_exec(move || {
            if root {
                rustix::thread::set_thread_groups(&[])?;
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
        argv.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn the_scope_argv_is_exactly_the_documented_command() {
        let identity = BackendIdentity::new(997, 996).unwrap();
        let argv = scope_argv(
            scope().as_str(),
            identity,
            false,
            std::path::Path::new("/trials/t1/c00"),
            &[CpuId::from(2), CpuId::from(18)],
            &["/bin/mprime".into(), "-t".into()],
        );
        assert_eq!(
            strings(&argv),
            [
                "systemd-run",
                "--scope",
                "--quiet",
                "--collect",
                "--unit",
                "togi-trial-t1-c00",
                "--uid",
                "997",
                "--gid",
                "996",
                "--working-directory",
                "/trials/t1/c00",
                "-p",
                "AllowedCPUs=2,18",
                "-p",
                "DefaultDependencies=no",
                "--",
                "/bin/mprime",
                "-t",
            ]
        );
    }

    #[test]
    fn a_user_manager_adds_the_user_flag_first() {
        let identity = BackendIdentity::new(1001, 1002).unwrap();
        let argv = scope_argv(
            scope().as_str(),
            identity,
            true,
            std::path::Path::new("/"),
            &[CpuId::from(0)],
            &["x".into()],
        );
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
            &[CpuId::from(0)],
            &plan,
        );
        assert_eq!(
            scoped.pin,
            [CpuId::from(0)],
            "a scoped launcher is pinned to the workload's CPUs, not the controller's"
        );
        let command = command_for(
            Confinement::Unscoped,
            &scope(),
            std::path::Path::new("/tmp"),
            &[CpuId::from(0)],
            &plan,
        );
        assert_eq!(strings(&command.argv), ["/bin/true", "a"]);
        assert_eq!(command.cwd, std::path::Path::new("/tmp"));
        assert_eq!(command.pin, [CpuId::from(0)]);
    }

    #[cfg(target_os = "linux")]
    mod attributes {
        use super::*;

        const ATTRIBUTES_HELPER: &str = "launch::tests::attributes::launch_attributes_helper";

        #[test]
        fn launch_attributes_helper() {
            if !std::env::args().any(|arg| arg == "helper:launch-attributes") {
                return;
            }
            for line in std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
            {
                if line.starts_with("Groups:") || line.starts_with("Uid:") {
                    println!("attr:{line}");
                }
            }
            std::process::exit(0);
        }

        fn status_lines(status: &str) -> Vec<String> {
            status
                .lines()
                .filter(|line| line.starts_with("Groups:") || line.starts_with("Uid:"))
                .map(|line| format!("attr:{line}"))
                .collect()
        }

        fn launched_attributes() -> Vec<String> {
            let command = LaunchCommand {
                argv: [
                    std::env::current_exe().unwrap().into_os_string(),
                    "--exact".into(),
                    ATTRIBUTES_HELPER.into(),
                    "--nocapture".into(),
                    "--test-threads=1".into(),
                    "helper:launch-attributes".into(),
                ]
                .to_vec(),
                cwd: "/".into(),
                pin: Vec::new(),
            };
            let child = spawn(&command).unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .filter_map(|line| line.find("attr:").map(|at| line[at..].to_owned()))
                .collect()
        }

        #[test]
        fn real_process_a_launcher_is_killed_when_its_forking_thread_dies() {
            use std::os::unix::process::ExitStatusExt as _;
            let command = LaunchCommand {
                argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 20".into()],
                cwd: "/".into(),
                pin: Vec::new(),
            };
            let mut child = std::thread::spawn(move || spawn(&command).unwrap())
                .join()
                .unwrap();
            let status = child.wait().unwrap();
            assert_eq!(
                status.signal(),
                Some(rustix::process::Signal::KILL.as_raw()),
                "{status:?}"
            );
        }

        #[test]
        fn real_process_an_unprivileged_launcher_keeps_its_credentials_and_never_attempts_setgroups()
         {
            if rustix::process::geteuid().is_root() {
                return;
            }
            let own = status_lines(&std::fs::read_to_string("/proc/self/status").unwrap());
            let attributes = launched_attributes();
            assert_eq!(attributes, own, "{attributes:?}");
        }

        #[test]
        fn real_process_a_root_launcher_clears_its_supplementary_groups() {
            if !rustix::process::geteuid().is_root() {
                return;
            }
            let attributes = launched_attributes();
            let groups = attributes
                .iter()
                .find_map(|line| line.strip_prefix("attr:Groups:"))
                .unwrap();
            assert_eq!(groups.trim(), "", "{attributes:?}");
        }
    }
}
