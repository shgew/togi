#![cfg(all(feature = "hardware", target_os = "linux"))]

//! Real systemd scopes. These need root, systemd and an unprivileged backend account named by
//! `TOGI_TRIAL_HARDWARE_USER`; the owner runs them as root under the host lock.

#[cfg(test)]
mod hardware {
    use std::ffi::OsString;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use togi_backend::{Backend, Error, FileName, Input, Line, Plan, Ready};
    use togi_machine::{
        CancellationToken, CcdId, Condition, CoreId, CoreInfo, CpuId, Regime, Reporter, Running,
        Signal, Stopped, TrialSpec, Trials, Workload, pick_workload,
    };
    use togi_trial::{BackendIdentity, Backends, Options, Runner};

    const HELPER: &str = "hardware::helper";

    #[test]
    fn helper() {
        let Some(arg) = std::env::args().find(|arg| arg.starts_with("helper:")) else {
            return;
        };
        let parts: Vec<&str> = arg["helper:".len()..].split(':').collect();
        run_mode(&parts);
        std::process::exit(0);
    }

    fn hour() {
        std::thread::sleep(Duration::from_secs(3600));
    }

    fn run_mode(parts: &[&str]) {
        let big = vec![b'x'; 1 << 20];
        match parts[0] {
            "pin" => {
                let cpu: usize = parts[1].parse().unwrap_or(0);
                let mut set = rustix::thread::CpuSet::new();
                set.set(cpu);
                match rustix::thread::sched_setaffinity(None, &set) {
                    Ok(()) => println!("pinned"),
                    Err(_) => println!("PIN FAILED"),
                }
                let _ = std::io::stdout().flush();
                hour();
            }
            "owner" => {
                let exe = std::env::current_exe().unwrap();
                #[expect(
                    clippy::disallowed_methods,
                    clippy::zombie_processes,
                    reason = "the helper starts a descendant that outlives it on purpose"
                )]
                let child = std::process::Command::new(exe)
                    .args(["--exact", HELPER, "--nocapture", "helper:sleep"])
                    .spawn()
                    .unwrap();
                std::fs::write("descendant.pid.tmp", child.id().to_string()).unwrap();
                std::fs::rename("descendant.pid.tmp", "descendant.pid").unwrap();
                hour();
            }
            "stdout" => {
                let _ = std::io::stdout().write_all(&big);
                hour();
            }
            "stderr" => {
                let _ = std::io::stderr().write_all(&big);
                hour();
            }
            "watched" => {
                std::fs::write("results.txt", &big).unwrap();
                hour();
            }
            "identity" => {
                let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
                let field = |name: &str| {
                    status
                        .lines()
                        .find_map(|line| line.strip_prefix(name))
                        .unwrap_or_default()
                        .trim()
                        .to_owned()
                };
                let report = serde_json::json!({
                    "uid": field("Uid:"),
                    "gid": field("Gid:"),
                    "groups": field("Groups:"),
                    "cwd": std::env::current_dir().unwrap(),
                    "created": std::fs::write("created.txt", "x").is_ok(),
                    "input": std::fs::OpenOptions::new().append(true).open("input.txt").is_ok(),
                    "log": std::fs::OpenOptions::new().append(true).open("stdout.log").is_ok(),
                    "parent": std::fs::write("../outside.txt", "x").is_ok(),
                });
                std::fs::write("identity.json.tmp", report.to_string()).unwrap();
                std::fs::rename("identity.json.tmp", "identity.json").unwrap();
                println!("IDENTITY READY");
                let _ = std::io::stdout().flush();
                hour();
            }
            _ => hour(),
        }
    }

    struct Staged {
        root: PathBuf,
        exe: PathBuf,
    }

    impl Staged {
        fn new() -> Self {
            assert!(
                rustix::process::geteuid().is_root(),
                "hardware tests run as root under the host lock"
            );
            let root = PathBuf::from(format!("/run/togi-trial-hardware-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
            let exe = root.join("helper");
            std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o555)).unwrap();
            Self { root, exe }
        }
    }

    impl Drop for Staged {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn account() -> BackendIdentity {
        let name =
            std::env::var("TOGI_TRIAL_HARDWARE_USER").unwrap_or_else(|_| "togi-trial".to_owned());
        BackendIdentity::lookup(&name).unwrap()
    }

    struct Helper {
        exe: PathBuf,
        mode: String,
        inputs: bool,
        fail_second_plan: bool,
        planned: std::sync::atomic::AtomicUsize,
    }

    impl Helper {
        fn new(staged: &Staged, mode: &str) -> Self {
            Self {
                exe: staged.exe.clone(),
                mode: mode.to_owned(),
                inputs: false,
                fail_second_plan: false,
                planned: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl Backend for Helper {
        fn kind(&self) -> togi_machine::Backend {
            togi_machine::Backend::Mprime
        }

        fn check(&self) -> Result<Ready, Error> {
            Ok(Ready {
                detail: "helper".to_owned(),
            })
        }

        fn plan(&self, workload: &Workload, _: &Path, cpus: &[CpuId]) -> Result<Plan, Error> {
            let planned = self
                .planned
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_second_plan && planned == 1 {
                return Err(Error::Unsupported {
                    backend: togi_machine::Backend::Mprime,
                    workload: workload.id,
                });
            }
            let mode = match self.mode.as_str() {
                "pin" => format!("pin:{}", cpus.last().map_or(0, |cpu| cpu.0 + 1)),
                other => other.to_owned(),
            };
            Ok(Plan {
                program: self.exe.clone(),
                args: ["--exact", HELPER, "--nocapture", &format!("helper:{mode}")]
                    .map(OsString::from)
                    .to_vec(),
                inputs: if self.inputs {
                    vec![Input {
                        name: FileName::from_static("input.txt"),
                        contents: b"input".to_vec(),
                    }]
                } else {
                    Vec::new()
                },
                watched: vec![FileName::from_static("results.txt")],
            })
        }

        fn classify(&self, line: &str) -> Line {
            if line.starts_with("PIN FAILED") {
                Line::SetupError
            } else {
                Line::Other
            }
        }
    }

    fn topology() -> Vec<CoreInfo> {
        (0..2)
            .map(|n| CoreInfo {
                core: CoreId::new(n).unwrap(),
                ccd: CcdId(0),
                cpus: vec![CpuId(u16::try_from(n).unwrap() + 2)],
            })
            .collect()
    }

    fn runner(staged: &Staged, backend: Helper) -> Runner {
        Runner::new(Options {
            dir: staged.root.join("trials"),
            backends: Backends::new().with(Arc::new(backend)),
            topology: topology(),
            identity: Some(account()),
            pm_table: None,
        })
        .unwrap()
    }

    fn spec(regime: Regime, cores: &[i64], duration: Duration) -> TrialSpec {
        let topology = topology();
        TrialSpec {
            id: "hardware".into(),
            regime,
            workload: pick_workload(regime, 0).clone(),
            condition: Condition::Alone,
            cores: cores.iter().map(|&n| CoreId::new(n).unwrap()).collect(),
            cpus: cores
                .iter()
                .map(|&n| topology[usize::try_from(n).unwrap()].cpus[0])
                .collect(),
            duration,
            index: 0,
            seed: 1,
        }
    }

    struct Quiet;

    impl Reporter for Quiet {
        fn progress(&mut self, _: &str) {}
        fn sample(&mut self, _: togi_machine::Sample) {}
        fn signal(&mut self, _: CoreId, _: Signal, _: &str) {}
    }

    fn ready() -> Stopped {
        #[expect(
            clippy::disallowed_methods,
            reason = "a hardware test stands in for the session's proof that nothing runs"
        )]
        Stopped::confirmed()
    }

    fn start(runner: &mut Runner, spec: TrialSpec) -> Box<dyn Running> {
        runner
            .start(&CancellationToken::new(), spec, ready())
            .map_err(|partial| partial.error)
            .unwrap()
    }

    fn scope_active(name: &str) -> bool {
        #[expect(
            clippy::disallowed_methods,
            reason = "a hardware test inspects systemd directly"
        )]
        let status = Command::new("systemctl")
            .args(["is-active", "--quiet", &format!("{name}.scope")])
            .status()
            .unwrap();
        status.success()
    }

    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit(')')
                    .next()
                    .map(|rest| rest.trim().starts_with(['Z', 'X']))
            })
            .is_some_and(|dead| !dead)
    }

    fn wait_for(limit: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + limit;
        while Instant::now() < end {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        condition()
    }

    #[test]
    fn a_helper_cannot_pin_outside_its_scope_and_the_scope_ends_with_the_trial() {
        let staged = Staged::new();
        let mut runner = runner(&staged, Helper::new(&staged, "pin"));
        let mut trial = start(&mut runner, spec(Regime::R1, &[0], Duration::from_secs(4)));
        let result = trial
            .wait(&CancellationToken::new(), &mut Quiet)
            .unwrap_or_else(|partial| partial.value);
        assert!(result.escaped.is_empty());
        assert!(
            result.inconclusive.is_some(),
            "a refused pin is a setup error"
        );
        assert!(trial.stop().is_ok());
        assert!(wait_for(Duration::from_secs(5), || !scope_active(
            "togi-trial-hardware"
        )));
    }

    #[test]
    fn a_detached_descendant_dies_with_its_scope() {
        let staged = Staged::new();
        let mut runner = runner(&staged, Helper::new(&staged, "owner"));
        let trial = start(&mut runner, spec(Regime::R1, &[0], Duration::from_secs(3)));
        let pid_file = staged.root.join("trials/hardware/work/descendant.pid");
        assert!(wait_for(Duration::from_secs(10), || pid_file.exists()));
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(trial.stop().is_ok());
        assert!(wait_for(Duration::from_secs(5), || !alive(pid)));
    }

    #[test]
    fn a_rolled_back_start_is_confirmed_and_both_scopes_end() {
        let staged = Staged::new();
        let mut backend = Helper::new(&staged, "owner");
        backend.fail_second_plan = true;
        let mut runner = runner(&staged, backend);
        let partial = runner
            .start(
                &CancellationToken::new(),
                spec(Regime::R7, &[0, 1], Duration::from_secs(30)),
                ready(),
            )
            .err()
            .unwrap();
        assert!(
            partial.value.is_some(),
            "the rollback confirmed nothing runs"
        );
        assert!(!scope_active("togi-trial-hardware-c00"));
    }

    #[test]
    fn oversized_output_on_every_source_is_inconclusive_and_the_scope_ends() {
        for mode in ["stdout", "stderr", "watched"] {
            let staged = Staged::new();
            let mut runner = runner(&staged, Helper::new(&staged, mode));
            let begun = Instant::now();
            let mut trial = start(&mut runner, spec(Regime::R1, &[0], Duration::from_secs(30)));
            let result = trial
                .wait(&CancellationToken::new(), &mut Quiet)
                .unwrap_or_else(|partial| partial.value);
            assert!(result.inconclusive.is_some(), "{mode}");
            assert!(trial.stop().is_ok(), "{mode}");
            assert!(begun.elapsed() < Duration::from_secs(20), "{mode}");
            assert!(wait_for(Duration::from_secs(5), || !scope_active(
                "togi-trial-hardware"
            )));
        }
    }

    #[test]
    fn a_backend_runs_as_the_account_with_no_groups_and_only_its_own_files_writable() {
        let staged = Staged::new();
        let mut backend = Helper::new(&staged, "identity");
        backend.inputs = true;
        let mut runner = runner(&staged, backend);
        let trial = start(&mut runner, spec(Regime::R1, &[0], Duration::from_secs(30)));
        let report = staged.root.join("trials/hardware/work/identity.json");
        assert!(wait_for(Duration::from_secs(10), || report.exists()));
        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
        let identity = account();
        assert!(
            report["uid"]
                .as_str()
                .unwrap()
                .starts_with(&identity.uid().to_string())
        );
        assert!(
            report["gid"]
                .as_str()
                .unwrap()
                .starts_with(&identity.gid().to_string())
        );
        assert_eq!(report["groups"].as_str().unwrap_or_default(), "");
        assert_eq!(report["created"], true);
        assert_eq!(report["input"], true);
        assert_eq!(report["log"], false);
        assert_eq!(report["parent"], false);
        assert!(trial.stop().is_ok());
    }

    #[test]
    fn the_sweep_removes_orphaned_trial_scopes_and_keeps_every_other_scope() {
        let staged = Staged::new();
        let mut first = runner(&staged, Helper::new(&staged, "owner"));
        let trial = start(&mut first, spec(Regime::R1, &[0], Duration::from_secs(600)));
        let pid_file = staged.root.join("trials/hardware/work/descendant.pid");
        assert!(wait_for(Duration::from_secs(10), || pid_file.exists()));
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "a hardware test creates an unrelated scope to prove the sweep leaves it alone"
        )]
        let unrelated = Command::new("systemd-run")
            .args([
                "--scope",
                "--quiet",
                "--unit",
                "togi-unrelated-hardware",
                "sleep",
                "300",
            ])
            .spawn()
            .unwrap();
        std::mem::forget(trial);
        drop(first);
        let mut second = runner(&staged, Helper::new(&staged, "owner"));
        let swept = second.sweep(&CancellationToken::new()).unwrap();
        assert!(swept.detail.contains("no trial unit or process remains"));
        assert!(wait_for(Duration::from_secs(5), || !alive(pid)));
        assert!(scope_active("togi-unrelated-hardware"));
        drop(unrelated);
    }
}
