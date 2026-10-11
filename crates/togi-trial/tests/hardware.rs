#![cfg(all(feature = "hardware", target_os = "linux"))]

//! Real systemd scopes. These need root, systemd and an unprivileged backend account named by
//! `TOGI_TRIAL_HARDWARE_USER`; the owner runs them as root under the host lock.

#[cfg(test)]
mod hardware {
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{Read as _, Write as _};
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
    use std::time::{Duration, Instant};

    use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags};
    use rustix::io::Errno;
    use togi_backend::{Backend, Error, FileName, Input, Line, Plan, Ready};
    use togi_hostlock::{DEFAULT_PATH, HostLock, Policy};
    use togi_machine::{
        CancellationToken, CcdId, Condition, CoreId, CoreInfo, CpuId, Regime, Reporter, Running,
        Signal, Stopped, TrialSpec, Trials, Workload, pick_workload,
    };
    use togi_trial::{BackendIdentity, Backends, Options, Runner};

    const HELPER: &str = "hardware::helper";
    const ROOT_ENV: &str = "TOGI_TRIAL_HARDWARE_ROOT";
    const TEARDOWN: Duration = Duration::from_secs(15);
    const LINE_LIMIT: usize = 64 * 1024;
    const REPORT_LIMIT: usize = 64 * 1024;
    const WORK: &[&str] = &["trials", "hardware", "work"];
    const DIR_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    const WRITTEN: [&str; 2] = ["created.txt", "input.txt"];
    const DENIED: [&str; 8] = [
        "stdout.log",
        "stderr.log",
        "../samples.jsonl",
        "../retained",
        "../passed",
        "../../control",
        "../../events.jsonl",
        "../../outside",
    ];

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

    /// Writes `name` in the working directory so that it appears whole.
    fn publish(name: &str, contents: &[u8]) {
        let staging = format!("{name}.tmp");
        std::fs::write(&staging, contents).unwrap();
        std::fs::rename(&staging, name).unwrap();
    }

    fn publish_cgroup() {
        publish(
            "cgroup.txt",
            &std::fs::read("/proc/self/cgroup").unwrap_or_default(),
        );
    }

    fn attempt_write(path: &str) -> &'static str {
        let written = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o644)
            .open(path)
            .and_then(|mut file| file.write_all(b"backend write\n"));
        if written.is_ok() { "written" } else { "denied" }
    }

    fn run_mode(parts: &[&str]) {
        let big = vec![b'x'; 1 << 20];
        match parts[0] {
            "pin" => {
                publish_cgroup();
                let cpu: usize = parts[1].parse().unwrap_or(0);
                let mut set = rustix::thread::CpuSet::new();
                set.set(cpu);
                match rustix::thread::sched_setaffinity(None, &set) {
                    Ok(()) => println!("\npinned"),
                    Err(_) => println!("\nPIN FAILED"),
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
                let _descendant = std::process::Command::new(exe)
                    .args(["--exact", HELPER, "--nocapture", "helper:detached"])
                    .spawn()
                    .unwrap();
                hour();
            }
            "detached" => {
                rustix::process::setsid().unwrap();
                publish("descendant.pid", std::process::id().to_string().as_bytes());
                hour();
            }
            "stdout" => {
                publish_cgroup();
                let mut stdout = std::io::stdout();
                let _ = stdout
                    .write_all(b"\n")
                    .and_then(|()| stdout.write_all(&big));
                hour();
            }
            "stderr" => {
                publish_cgroup();
                let mut stderr = std::io::stderr();
                let _ = stderr
                    .write_all(b"\n")
                    .and_then(|()| stderr.write_all(&big));
                hour();
            }
            "watched" => {
                publish_cgroup();
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
                let writes: serde_json::Map<String, serde_json::Value> = WRITTEN
                    .into_iter()
                    .chain(DENIED)
                    .map(|path| (path.to_owned(), attempt_write(path).into()))
                    .collect();
                let report = serde_json::json!({
                    "uid": field("Uid:"),
                    "gid": field("Gid:"),
                    "groups": field("Groups:"),
                    "cwd": std::env::current_dir().unwrap(),
                    "writes": writes,
                });
                publish("identity.json", report.to_string().as_bytes());
                println!("\nIDENTITY READY");
                let _ = std::io::stdout().flush();
                hour();
            }
            "abandon" => {
                let root = std::env::var_os(ROOT_ENV).unwrap();
                let tree = Arc::new(Tree::open(PathBuf::from(root)));
                let mut runner = runner(&tree, Helper::new(&tree, "owner"));
                let _trial = start(
                    &mut runner,
                    spec(Regime::R1, &[0], Duration::from_secs(600)),
                );
                tree.create(&[], "abandoned", b"");
                hour();
            }
            _ => hour(),
        }
    }

    /// A directory tree reached only through descriptors opened without following symlinks.
    struct Tree {
        root: PathBuf,
        fd: OwnedFd,
    }

    impl Tree {
        fn open(root: PathBuf) -> Self {
            let fd = rustix::fs::openat(CWD, &root, DIR_FLAGS, Mode::empty()).unwrap();
            Self { root, fd }
        }

        fn dir(&self, dirs: &[&str]) -> rustix::io::Result<OwnedFd> {
            let mut dir = rustix::fs::openat(&self.fd, ".", DIR_FLAGS, Mode::empty())?;
            for name in dirs {
                dir = rustix::fs::openat(&dir, *name, DIR_FLAGS, Mode::empty())?;
            }
            Ok(dir)
        }

        /// The contents of `name` below `dirs`, or none while it does not exist yet. A symlink,
        /// FIFO or other nonregular file, or one larger than `limit`, fails the test.
        fn read(&self, dirs: &[&str], name: &str, limit: usize) -> Option<Vec<u8>> {
            let opened = self.dir(dirs).and_then(|dir| {
                rustix::fs::openat(
                    &dir,
                    name,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
            });
            let fd = match opened {
                Ok(fd) => fd,
                Err(Errno::NOENT) => return None,
                Err(errno) => panic!("open {}/{name}: {errno}", dirs.join("/")),
            };
            let stat = rustix::fs::fstat(&fd).unwrap();
            assert_eq!(
                FileType::from_raw_mode(stat.st_mode),
                FileType::RegularFile,
                "{name} is not a regular file"
            );
            let mut bytes = Vec::new();
            File::from(fd)
                .take(u64::try_from(limit).unwrap() + 1)
                .read_to_end(&mut bytes)
                .unwrap();
            assert!(bytes.len() <= limit, "{name} is larger than {limit} bytes");
            Some(bytes)
        }

        fn create(&self, dirs: &[&str], name: &str, contents: &[u8]) {
            let dir = self.dir(dirs).unwrap();
            let fd = rustix::fs::openat(
                &dir,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o644),
            )
            .unwrap();
            rustix::fs::fchmod(&fd, Mode::from_raw_mode(0o644)).unwrap();
            File::from(fd).write_all(contents).unwrap();
        }

        fn owner(&self, dirs: &[&str], name: &str) -> (u32, u32) {
            let stat = rustix::fs::statat(self.dir(dirs).unwrap(), name, AtFlags::SYMLINK_NOFOLLOW)
                .unwrap();
            (stat.st_uid, stat.st_gid)
        }

        fn exists(&self, dirs: &[&str], name: &str) -> bool {
            match rustix::fs::statat(self.dir(dirs).unwrap(), name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => true,
                Err(Errno::NOENT) => false,
                Err(errno) => panic!("stat {}/{name}: {errno}", dirs.join("/")),
            }
        }
    }

    static SERIAL: Mutex<()> = Mutex::new(());

    /// A fresh staging directory with the helper and an empty `trials`, with the host lock held
    /// until the directory is removed.
    struct Staged {
        tree: Arc<Tree>,
        _dir: tempfile::TempDir,
        _lock: HostLock,
        _serial: MutexGuard<'static, ()>,
    }

    impl Staged {
        fn new() -> Self {
            let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
            let lock = HostLock::acquire(Path::new(DEFAULT_PATH), Policy::Host).unwrap();
            let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
            let parent_fd = rustix::fs::openat(
                CWD,
                &parent,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .unwrap();
            let euid = rustix::process::geteuid().as_raw();
            let parent_stat = rustix::fs::fstat(&parent_fd).unwrap();
            assert!(
                (parent_stat.st_uid == 0 || parent_stat.st_uid == euid)
                    && (parent_stat.st_mode & 0o022 == 0 || parent_stat.st_mode & 0o1000 != 0),
                "{} must belong to root or the caller and be sticky if others can write to it",
                parent.display()
            );
            let dir = tempfile::Builder::new()
                .prefix("togi-trial-hardware-")
                .tempdir_in(&parent)
                .unwrap();
            let name = dir.path().file_name().unwrap();
            let fd = rustix::fs::openat(&parent_fd, name, DIR_FLAGS, Mode::empty()).unwrap();
            let stat = rustix::fs::fstat(&fd).unwrap();
            assert_eq!(stat.st_uid, euid, "the staging directory is the caller's");
            rustix::fs::fchmod(&fd, Mode::from_raw_mode(0o700)).unwrap();
            let tree = Tree {
                root: parent.join(name),
                fd,
            };
            let mut source = File::open(std::env::current_exe().unwrap()).unwrap();
            let mut helper = File::from(
                rustix::fs::openat(
                    &tree.fd,
                    "helper",
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o700),
                )
                .unwrap(),
            );
            std::io::copy(&mut source, &mut helper).unwrap();
            rustix::fs::fchmod(&helper, Mode::from_raw_mode(0o555)).unwrap();
            drop(helper);
            rustix::fs::mkdirat(&tree.fd, "trials", Mode::from_raw_mode(0o755)).unwrap();
            rustix::fs::fchmod(tree.dir(&["trials"]).unwrap(), Mode::from_raw_mode(0o755)).unwrap();
            rustix::fs::fchmod(&tree.fd, Mode::from_raw_mode(0o755)).unwrap();
            Self {
                tree: Arc::new(tree),
                _dir: dir,
                _lock: lock,
                _serial: serial,
            }
        }
    }

    fn config() -> Option<togi_config::Config> {
        togi_config::Config::load(Path::new(togi_config::DEFAULT_PATH)).ok()
    }

    fn root() -> bool {
        rustix::process::geteuid().is_root()
    }

    /// As root, the configured backend account (or `TOGI_TRIAL_HARDWARE_USER`); otherwise the
    /// caller, whose systemd user manager runs the scopes.
    fn account() -> BackendIdentity {
        if !root() {
            let uid = rustix::process::geteuid().as_raw();
            let gid = rustix::process::getegid().as_raw();
            return BackendIdentity::new(uid, gid).unwrap();
        }
        let name = std::env::var("TOGI_TRIAL_HARDWARE_USER")
            .ok()
            .or_else(|| config().and_then(|config| config.backend_user().map(str::to_owned)));
        BackendIdentity::lookup(&name.expect("set TOGI_TRIAL_HARDWARE_USER or backend_user"))
            .unwrap()
    }

    #[derive(Clone, Copy, Debug)]
    struct Process {
        pid: u32,
        start: u64,
    }

    struct Stat {
        state: char,
        ppid: u32,
        pgrp: u32,
        session: u32,
        start: u64,
    }

    fn stat(pid: u32) -> Option<Stat> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<&str> = text.rsplit_once(')')?.1.split_whitespace().collect();
        Some(Stat {
            state: fields.first()?.chars().next()?,
            ppid: fields.get(1)?.parse().ok()?,
            pgrp: fields.get(2)?.parse().ok()?,
            session: fields.get(3)?.parse().ok()?,
            start: fields.get(19)?.parse().ok()?,
        })
    }

    fn names_scope(cgroup: &str, scope: &str) -> bool {
        let suffix = format!("/{scope}.scope");
        cgroup.lines().any(|line| line.ends_with(&suffix))
    }

    impl Process {
        fn of(pid: u32) -> Option<(Self, Stat)> {
            let stat = stat(pid)?;
            Some((
                Self {
                    pid,
                    start: stat.start,
                },
                stat,
            ))
        }

        fn alive(self) -> bool {
            stat(self.pid)
                .is_some_and(|stat| stat.start == self.start && !matches!(stat.state, 'Z' | 'X'))
        }

        fn in_scope(self, scope: &str) -> bool {
            std::fs::read_to_string(format!("/proc/{}/cgroup", self.pid))
                .is_ok_and(|cgroup| names_scope(&cgroup, scope))
        }
    }

    /// The `owner` helper's launcher and the descendant it detached into a session of its own.
    struct Detached {
        launcher: Process,
        descendant: Process,
    }

    /// Waits for the descendant in `instance` and checks that it left the launcher's session and
    /// process group while both stay in `scope`.
    fn detached(tree: &Tree, instance: &[&str], scope: &str) -> Result<Detached, String> {
        let mut report = None;
        if !wait_for(Duration::from_secs(10), || {
            report = tree.read(instance, "descendant.pid", REPORT_LIMIT);
            report.is_some()
        }) {
            return Err(format!("no descendant.pid in {}", instance.join("/")));
        }
        let text = String::from_utf8(report.unwrap_or_default()).map_err(|e| e.to_string())?;
        let pid: u32 = text
            .trim()
            .parse()
            .map_err(|error| format!("descendant.pid {text:?}: {error}"))?;
        let (descendant, own) =
            Process::of(pid).ok_or_else(|| "the descendant is gone".to_owned())?;
        let (launcher, parent) =
            Process::of(own.ppid).ok_or_else(|| "the launcher is gone".to_owned())?;
        if own.session != pid || parent.session == pid || parent.pgrp == own.pgrp {
            return Err(format!(
                "descendant {pid} (group {}, session {}) did not detach from launcher {} (group {}, session {})",
                own.pgrp, own.session, launcher.pid, parent.pgrp, parent.session
            ));
        }
        for process in [launcher, descendant] {
            if !process.in_scope(scope) {
                return Err(format!("process {} is not in {scope}.scope", process.pid));
            }
        }
        Ok(Detached {
            launcher,
            descendant,
        })
    }

    /// When the second instance is planned: the first instance's verified descendant, and when
    /// its directory was moved away so that the second launch fails.
    struct Displaced {
        first: Detached,
        at: Instant,
    }

    struct Helper {
        tree: Arc<Tree>,
        mode: &'static str,
        displace_second: bool,
        planned: AtomicUsize,
        displaced: Mutex<Option<Result<Displaced, String>>>,
    }

    impl Helper {
        fn new(tree: &Arc<Tree>, mode: &'static str) -> Arc<Self> {
            Arc::new(Self::build(tree, mode, false))
        }

        fn build(tree: &Arc<Tree>, mode: &'static str, displace_second: bool) -> Self {
            Self {
                tree: Arc::clone(tree),
                mode,
                displace_second,
                planned: AtomicUsize::new(0),
                displaced: Mutex::new(None),
            }
        }

        /// Verifies the first instance's detached descendant, then moves the second instance's
        /// directory away, so its launcher cannot enter it and the launch fails after its scope
        /// was recorded.
        fn displace(&self) -> Result<Displaced, String> {
            let first = detached(
                &self.tree,
                &["trials", "hardware", "c00"],
                "togi-trial-hardware-c00",
            )?;
            let trial = self
                .tree
                .dir(&["trials", "hardware"])
                .map_err(|errno| errno.to_string())?;
            rustix::fs::renameat(&trial, "c01", &self.tree.fd, "displaced-c01")
                .map_err(|errno| errno.to_string())?;
            Ok(Displaced {
                first,
                at: Instant::now(),
            })
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
            let planned = self.planned.fetch_add(1, Ordering::SeqCst);
            if self.displace_second && planned == 1 {
                let displaced = self.displace();
                let failed = displaced.is_err();
                *self
                    .displaced
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(displaced);
                if failed {
                    return Err(Error::Unsupported {
                        backend: togi_machine::Backend::Mprime,
                        workload: workload.id(),
                    });
                }
            }
            let identity = self.mode == "identity";
            if identity {
                self.tree
                    .create(&["trials", "hardware"], "retained", b"prepared input\n");
            }
            let mode = match self.mode {
                "pin" => format!("pin:{}", cpus.last().map_or(0, |cpu| cpu.0 + 1)),
                other => other.to_owned(),
            };
            Ok(Plan {
                program: self.tree.root.join("helper"),
                args: ["--exact", HELPER, "--nocapture", &format!("helper:{mode}")]
                    .map(OsString::from)
                    .to_vec(),
                inputs: if identity {
                    vec![Input {
                        name: FileName::from_static("input.txt"),
                        contents: b"prepared input\n".to_vec(),
                    }]
                } else {
                    Vec::new()
                },
                watched: vec![FileName::from_static("results.txt")],
            })
        }

        fn classify(&self, line: &str) -> Line {
            if line == "PIN FAILED" {
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

    fn runner(tree: &Tree, backend: Arc<Helper>) -> Runner {
        Runner::new(Options {
            dir: tree.root.join("trials"),
            backends: Backends::new().with(backend),
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
            workload: pick_workload(regime, 0).id(),
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

    fn systemctl(args: &[&str]) -> Result<togi_command::Output, togi_command::Error> {
        let mut invocation = togi_command::Invocation::new("systemctl");
        if !root() {
            invocation = invocation.arg("--user");
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "a hardware test inspects and stops systemd units directly, within a bound"
        )]
        let output = togi_command::run_blocking(&invocation.args(args), Duration::from_secs(10));
        output
    }

    fn scope_active(scope: &str) -> bool {
        systemctl(&["is-active", "--quiet", &format!("{scope}.scope")])
            .unwrap()
            .status
            .success()
    }

    fn scope_listed(scope: &str) -> bool {
        let listed = systemctl(&[
            "list-units",
            "--all",
            "--plain",
            "--no-legend",
            &format!("{scope}.scope"),
        ])
        .unwrap();
        assert!(listed.status.success(), "systemctl list-units failed");
        !listed.stdout.is_empty()
    }

    fn scope_gone(scope: &str) -> bool {
        wait_for(Duration::from_secs(5), || !scope_listed(scope))
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

    fn chain(error: &dyn std::error::Error) -> String {
        let mut text = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            text.push_str(": ");
            text.push_str(&cause.to_string());
            source = cause.source();
        }
        text
    }

    /// A process the test started and the scope it leaves behind: on drop, even while unwinding,
    /// the process is killed and reaped and the scope stopped, each within a bound.
    struct Stray {
        unit: String,
        child: Option<Child>,
        ended: bool,
    }

    impl Stray {
        /// An unrelated scope that the sweep must leave alone.
        fn scope(name: &str) -> Self {
            let mut argv: Vec<&str> = Vec::new();
            if !root() {
                argv.push("--user");
            }
            argv.extend(["--scope", "--quiet", "--collect", "--unit", name]);
            argv.extend(["sleep", "300"]);
            #[expect(
                clippy::disallowed_methods,
                reason = "a hardware test creates an unrelated scope to prove the sweep leaves it alone"
            )]
            let child = Command::new("systemd-run")
                .args(argv)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let stray = Self {
                unit: name.to_owned(),
                child: Some(child),
                ended: false,
            };
            assert!(
                wait_for(Duration::from_secs(5), || scope_active(name)),
                "{name}.scope never became active"
            );
            stray
        }

        fn kill(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }

        /// Kills the process and stops the scope; true when the scope is then gone.
        fn end(mut self) -> bool {
            self.ended = true;
            self.kill();
            let _ = systemctl(&["stop", &format!("{}.scope", self.unit)]);
            scope_gone(&self.unit)
        }
    }

    impl Drop for Stray {
        fn drop(&mut self) {
            if !self.ended {
                self.kill();
                let _ = systemctl(&["stop", &format!("{}.scope", self.unit)]);
            }
        }
    }

    #[test]
    fn a_helper_cannot_pin_outside_its_scope_and_the_scope_ends_with_the_trial() {
        let staged = Staged::new();
        let mut runner = runner(&staged.tree, Helper::new(&staged.tree, "pin"));
        let mut trial = start(&mut runner, spec(Regime::R1, &[0], Duration::from_secs(30)));
        let result = trial
            .wait(&CancellationToken::new(), &mut Quiet)
            .unwrap_or_else(|partial| partial.value);
        assert!(result.escaped.is_empty(), "{result:?}");
        assert!(result.failure.is_none(), "{result:?}");
        assert_eq!(
            result.inconclusive.as_deref(),
            Some("mprime setup error: PIN FAILED"),
            "the helper's refused pin is the reason"
        );
        let cgroup = staged.tree.read(WORK, "cgroup.txt", REPORT_LIMIT).unwrap();
        assert!(
            names_scope(&String::from_utf8_lossy(&cgroup), "togi-trial-hardware"),
            "the helper ran in its scope"
        );
        assert!(trial.stop().is_ok());
        assert!(scope_gone("togi-trial-hardware"));
    }

    #[test]
    fn a_detached_descendant_dies_with_its_scope() {
        let staged = Staged::new();
        let mut runner = runner(&staged.tree, Helper::new(&staged.tree, "owner"));
        let trial = start(&mut runner, spec(Regime::R1, &[0], Duration::from_secs(30)));
        let detached = detached(&staged.tree, WORK, "togi-trial-hardware").unwrap();
        let begun = Instant::now();
        assert!(trial.stop().is_ok());
        assert!(begun.elapsed() <= TEARDOWN);
        assert!(
            !detached.descendant.alive(),
            "the detached descendant survived"
        );
        assert!(!detached.launcher.alive(), "the launcher survived");
        assert!(scope_gone("togi-trial-hardware"));
    }

    #[test]
    fn a_rolled_back_start_is_confirmed_and_both_scopes_end() {
        let staged = Staged::new();
        let helper = Arc::new(Helper::build(&staged.tree, "owner", true));
        let mut runner = runner(&staged.tree, Arc::clone(&helper));
        let partial = runner
            .start(
                &CancellationToken::new(),
                spec(Regime::R7, &[0, 1], Duration::from_secs(30)),
                ready(),
            )
            .err()
            .expect("the second launch fails");
        let returned = Instant::now();
        let displaced = helper
            .displaced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .expect("the second instance was planned")
            .unwrap();
        assert!(returned - displaced.at <= TEARDOWN, "the rollback overran");
        let error = chain(&partial.error);
        assert!(
            error.contains("launch instance c01 in scope togi-trial-hardware-c01"),
            "{error}"
        );
        assert!(
            !matches!(partial.error, togi_machine::Error::Containment(_)),
            "{error}"
        );
        assert!(
            partial.value.is_some(),
            "the rollback confirmed nothing runs: {error}"
        );
        assert!(
            !displaced.first.descendant.alive(),
            "the pipe holder survived"
        );
        assert!(
            !displaced.first.launcher.alive(),
            "the first launcher survived"
        );
        assert!(scope_gone("togi-trial-hardware-c00"));
        assert!(scope_gone("togi-trial-hardware-c01"));
    }

    #[test]
    fn oversized_output_on_every_source_is_inconclusive_and_the_scope_ends() {
        for (mode, source) in [
            ("stdout", "stdout.log"),
            ("stderr", "stderr.log"),
            ("watched", "watched output"),
        ] {
            let staged = Staged::new();
            let mut runner = runner(&staged.tree, Helper::new(&staged.tree, mode));
            let begun = Instant::now();
            let mut trial = start(&mut runner, spec(Regime::R1, &[0], Duration::from_secs(30)));
            let result = trial
                .wait(&CancellationToken::new(), &mut Quiet)
                .unwrap_or_else(|partial| partial.value);
            assert_eq!(
                result.inconclusive.as_deref(),
                Some(&*format!(
                    "core 00 read {source}: backend output line exceeds 64 KiB"
                )),
                "{mode}"
            );
            assert!(
                result.failure.is_none() && result.escaped.is_empty(),
                "{mode}"
            );
            let cgroup = staged.tree.read(WORK, "cgroup.txt", REPORT_LIMIT).unwrap();
            assert!(
                names_scope(&String::from_utf8_lossy(&cgroup), "togi-trial-hardware"),
                "{mode} ran in its scope"
            );
            assert!(trial.stop().is_ok(), "{mode}");
            assert!(begun.elapsed() < Duration::from_secs(20), "{mode}");
            if mode != "watched" {
                let log = staged.tree.read(WORK, source, 2 * LINE_LIMIT).unwrap();
                let last = log.rsplit(|&byte| byte == b'\n').next().unwrap_or_default();
                assert!(
                    last.len() == LINE_LIMIT && last.iter().all(|&byte| byte == b'x'),
                    "{mode} kept {} bytes of the oversized line",
                    last.len()
                );
            }
            assert!(scope_gone("togi-trial-hardware"), "{mode}");
        }
    }

    #[test]
    fn a_backend_runs_as_the_account_with_no_groups_and_only_its_own_files_writable() {
        let staged = Staged::new();
        let tree = &staged.tree;
        for name in ["control", "events.jsonl"] {
            tree.create(&["trials"], name, b"protected parent\n");
        }
        let mut runner = runner(tree, Helper::new(tree, "identity"));
        let trial = start(&mut runner, spec(Regime::R1, &[0], Duration::from_secs(30)));
        let mut report = None;
        assert!(wait_for(Duration::from_secs(10), || {
            report = tree.read(WORK, "identity.json", REPORT_LIMIT);
            report.is_some()
        }));
        let report: serde_json::Value = serde_json::from_slice(&report.unwrap()).unwrap();
        let identity = account();
        let ids = |field: &str| -> Vec<String> {
            report[field]
                .as_str()
                .unwrap()
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(ids("uid"), vec![identity.uid().to_string(); 4]);
        assert_eq!(ids("gid"), vec![identity.gid().to_string(); 4]);
        assert_eq!(report["groups"].as_str().unwrap_or_default(), "");
        assert_eq!(
            report["cwd"].as_str().map(PathBuf::from),
            Some(tree.root.join("trials/hardware/work"))
        );
        for path in WRITTEN {
            assert_eq!(report["writes"][path], "written", "{path}");
        }
        for path in DENIED {
            assert_eq!(report["writes"][path], "denied", "{path}");
        }
        assert_eq!(
            tree.read(WORK, "created.txt", REPORT_LIMIT).unwrap(),
            b"backend write\n"
        );
        assert_eq!(
            tree.read(WORK, "input.txt", REPORT_LIMIT).unwrap(),
            b"prepared input\nbackend write\n"
        );
        let runner_owned = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        let backend_owned = (identity.uid(), identity.gid());
        for (dirs, name, owner) in [
            (&[][..], "trials", runner_owned),
            (&["trials"][..], "hardware", runner_owned),
            (&["trials"][..], "control", runner_owned),
            (&["trials"][..], "events.jsonl", runner_owned),
            (&["trials", "hardware"][..], "retained", runner_owned),
            (WORK, "stdout.log", runner_owned),
            (WORK, "stderr.log", runner_owned),
            (&["trials", "hardware"][..], "work", backend_owned),
            (WORK, "created.txt", backend_owned),
            (WORK, "input.txt", backend_owned),
        ] {
            assert_eq!(tree.owner(dirs, name), owner, "{}/{name}", dirs.join("/"));
        }
        assert!(!tree.exists(&["trials", "hardware"], "passed"));
        assert!(!tree.exists(&["trials"], "outside"));
        for name in ["control", "events.jsonl"] {
            assert_eq!(
                tree.read(&["trials"], name, REPORT_LIMIT).unwrap(),
                b"protected parent\n",
                "{name}"
            );
        }
        assert_eq!(
            tree.read(&["trials", "hardware"], "retained", REPORT_LIMIT)
                .unwrap(),
            b"prepared input\n"
        );
        let begun = Instant::now();
        assert!(trial.stop().is_ok());
        assert!(begun.elapsed() <= TEARDOWN);
        assert!(scope_gone("togi-trial-hardware"));
    }

    #[test]
    fn the_sweep_removes_orphaned_trial_scopes_and_keeps_every_other_scope() {
        let staged = Staged::new();
        let tree = &staged.tree;
        #[expect(
            clippy::disallowed_methods,
            reason = "a hardware test runs a trial runner in a process it then kills, as a crash would"
        )]
        let owner = Command::new(tree.root.join("helper"))
            .args(["--exact", HELPER, "--nocapture", "helper:abandon"])
            .env(ROOT_ENV, &tree.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut orphaned = Stray {
            unit: "togi-trial-hardware".to_owned(),
            child: Some(owner),
            ended: false,
        };
        let orphan = detached(tree, WORK, "togi-trial-hardware").unwrap();
        assert!(wait_for(Duration::from_secs(60), || tree.exists(&[], "abandoned")));
        orphaned.kill();
        let unrelated = Stray::scope("togi-unrelated-hardware");
        let preflight = Stray::scope("togi-preflight-hardware");
        assert!(orphan.descendant.alive(), "the orphan died with its runner");
        assert!(orphan.descendant.in_scope("togi-trial-hardware"));
        assert!(scope_active("togi-trial-hardware"));
        let mut runner = runner(tree, Helper::new(tree, "owner"));
        let begun = Instant::now();
        let swept = runner.sweep(&CancellationToken::new()).unwrap();
        assert!(begun.elapsed() <= TEARDOWN, "the sweep overran");
        assert!(
            swept.detail.starts_with("removed ") && !swept.detail.starts_with("removed 0 "),
            "{}",
            swept.detail
        );
        assert!(!orphan.descendant.alive(), "the orphan survived the sweep");
        assert!(!orphan.launcher.alive(), "the launcher survived the sweep");
        assert!(scope_gone("togi-trial-hardware"));
        assert!(scope_active("togi-unrelated-hardware"));
        assert!(scope_active("togi-preflight-hardware"));
        assert!(unrelated.end(), "the unrelated scope did not stop");
        assert!(preflight.end(), "the preflight scope did not stop");
        drop(orphaned);
    }

    struct Logged;

    impl Reporter for Logged {
        fn progress(&mut self, detail: &str) {
            eprintln!("progress: {detail}");
        }

        fn sample(&mut self, sample: togi_machine::Sample) {
            eprintln!("sample: {sample:?}");
        }

        fn signal(&mut self, core: CoreId, signal: Signal, detail: &str) {
            eprintln!("signal: core {core:02} {signal}: {detail}");
        }
    }

    /// The installed package of `kind`: from `env`, else from the configuration file.
    fn package(kind: togi_machine::Backend, env: &str) -> PathBuf {
        if let Some(path) = std::env::var_os(env) {
            return PathBuf::from(path);
        }
        let configured = config().and_then(|config| {
            let backends = config.backends();
            match kind {
                togi_machine::Backend::Mprime => backends.mprime().map(Path::to_path_buf),
                togi_machine::Backend::Ycruncher => backends.ycruncher().map(Path::to_path_buf),
            }
        });
        configured.unwrap_or_else(|| panic!("set {env}, or backends.{kind} in the configuration"))
    }

    #[test]
    fn every_r1_and_r2_workload_passes_twenty_seconds_on_cpu_2() {
        let staged = Staged::new();
        let backends: [(togi_machine::Backend, Arc<dyn Backend>); 2] = [
            (
                togi_machine::Backend::Mprime,
                Arc::new(
                    togi_mprime::Mprime::new(package(togi_machine::Backend::Mprime, "TOGI_MPRIME"))
                        .unwrap(),
                ),
            ),
            (
                togi_machine::Backend::Ycruncher,
                Arc::new(
                    togi_ycruncher::Ycruncher::new(package(
                        togi_machine::Backend::Ycruncher,
                        "TOGI_YCRUNCHER",
                    ))
                    .unwrap(),
                ),
            ),
        ];
        for (kind, backend) in backends {
            eprintln!("{kind}: {}", backend.check().unwrap().detail);
            let mut runner = Runner::new(Options {
                dir: staged.tree.root.join(format!("trials-{kind}")),
                backends: Backends::new().with(Arc::clone(&backend)),
                topology: vec![CoreInfo {
                    core: CoreId::new(2).unwrap(),
                    ccd: CcdId(0),
                    cpus: vec![CpuId(2)],
                }],
                identity: Some(account()),
                pm_table: None,
            })
            .unwrap();
            for regime in [Regime::R1, Regime::R2] {
                for (index, workload) in togi_machine::workloads(regime).iter().enumerate() {
                    if workload.backend != kind {
                        continue;
                    }
                    let spec = TrialSpec {
                        id: format!("hw-{}", workload.id),
                        regime,
                        workload: workload.id(),
                        condition: Condition::Alone,
                        cores: vec![CoreId::new(2).unwrap()],
                        cpus: vec![CpuId(2)],
                        duration: Duration::from_secs(20),
                        index,
                        seed: 0,
                    };
                    let mut trial = start(&mut runner, spec);
                    let result = trial
                        .wait(&CancellationToken::new(), &mut Logged)
                        .unwrap_or_else(|partial| panic!("{}: {}", workload.id, partial.error));
                    eprintln!(
                        "{} {regime} ran {:?}, Tctl max {:?}",
                        workload.id, result.ran, result.tctl_max_c
                    );
                    assert!(trial.stop().is_ok(), "{}", workload.id);
                    assert!(
                        result.failure.is_none()
                            && result.inconclusive.is_none()
                            && result.escaped.is_empty()
                            && result.ran >= Duration::from_secs(20),
                        "{} did not pass: {result:?}",
                        workload.id
                    );
                }
            }
        }
    }
}
