#![cfg(target_os = "linux")]
//! The helper the `togi-emergency-stop` NixOS VM test installs as `togi`.
//!
//! The VM has no SMU, so this executable is `togi` with one seam replaced: its `System` opens a machine whose SMU
//! is a persistent file and whose trials are the shipped `togi-trial` runner on real systemd scopes. Everything else
//! is production code: the host lock, the `emergency-stop` command with its sweep, validation, zero-only write and
//! readback, and the module's `ExecStopPost` hook that calls it. The production executable has no such switches.
//!
//! The wrapper `togi` in the VM test runs this test binary as
//! `<binary> --exact vm_recovery::helper --nocapture -- togi <togi arguments>`; `workload` stands in for a stress
//! program. Under `cargo test` the `helper` test finds no `--` and returns.
//!
//! The VM test steers it through files in `/var/lib/togi-vm`:
//!
//! - `offsets`: the fake SMU's offset of every core, one line, persistent. A missing file reads -5, the baseline.
//! - `smu.log`: one line per SMU operation with the pid that made it, and the trial scopes alive at each zeroing.
//! - `mode`: what `togi run` does. `kill` starts a trial and waits, restoring the baseline on SIGTERM; `fail` starts
//!   a trial and exits 1; `deadend` writes trial offsets, restores the baseline and exits 15; `clean` restores the
//!   baseline and exits 0.
//! - `fault`: `validation` refuses the SMU validation, `sweep` fails the sweep, `readback` makes core 1 read -7 after
//!   whatever was written.
//! - `trial`: written once a trial runs.

#[cfg(test)]
mod vm_recovery {
    use std::ffi::OsString;
    use std::fs::OpenOptions;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::process::exit;
    use std::sync::Arc;
    use std::time::Duration;

    use jiff::Timestamp;
    use jiff::tz::TimeZone;
    use rustix::time::{ClockId, clock_gettime};
    use togi_backend::{Backend, Line, Plan, Ready};
    use togi_cli::dashboard::Surface;
    use togi_cli::system::{Real, System};
    use togi_config::Config;
    use togi_hardware::day::Observer;
    use togi_hardware::{Diagnosis, Error as HardwareError, RebootError};
    use togi_hostlock::{HostLock, Policy};
    use togi_journal::{Payload, SessionStart, Stamp, StateDir};
    use togi_machine::{
        BiosContext, BootId, CancellationToken, CcdId, Check, Clock, Condition, CoreId, CoreInfo,
        CoreRank, CpuId, Error, Host, Kernel, KernelCursor, KernelRead, Machine, Mce,
        ObservedOffset, Partial, PerCore, PstoreRecord, Regime, Reporter, ResetReason, Running,
        Signal, Smu, SmuValidated, Stopped, Swept, TargetOffset, TrialConditions, TrialRole,
        TrialSpec, Trials, Workload, pick_workload,
    };
    use togi_render::OutputFacts;
    use togi_trial::{BackendIdentity, Backends, Options, Runner};
    use togi_tuningboot::Bootloader;

    const STATE: &str = "/var/lib/togi-vm";
    const STATE_DIR: &str = "/var/lib/togi";
    const ACCOUNT: &str = "togi-trial";
    /// The unified cgroup hierarchy; trial scopes sit in whichever slice systemd gave them.
    const CGROUPS: &str = "/sys/fs/cgroup";
    /// What the fake SMU reads before anything wrote it: the BIOS baseline a normal stop restores.
    const BASELINE: i64 = -5;
    /// The offset of a loaded core during a trial; an unloaded core's is shallower and different for each, so the
    /// least-deep unloaded core is unambiguous: core 2 at -10 on a four-core machine.
    const TRIAL_OFFSET: i64 = -30;
    const LOADED: usize = 2;

    fn trial_offsets() -> Vec<i64> {
        (0..cores())
            .map(|core| {
                if core < LOADED {
                    TRIAL_OFFSET
                } else {
                    -10 * i64::try_from(core - 1).unwrap()
                }
            })
            .collect()
    }

    fn path(name: &str) -> PathBuf {
        Path::new(STATE).join(name)
    }

    fn text(name: &str) -> String {
        std::fs::read_to_string(path(name))
            .map(|contents| contents.trim().to_owned())
            .unwrap_or_default()
    }

    fn other(detail: impl Into<String>) -> Error {
        Error::Other(detail.into().into())
    }

    /// The cores of the VM, which its test configures with four vCPUs. A constant, not the process's affinity, which
    /// togi narrows to the unloaded cores during a trial.
    const CORES: usize = 4;

    const fn cores() -> usize {
        CORES
    }

    fn topology() -> Vec<CoreInfo> {
        (0..cores())
            .map(|n| CoreInfo {
                core: CoreId::new(i64::try_from(n).unwrap()).unwrap(),
                ccd: CcdId::new(0).unwrap(),
                cpus: vec![CpuId::from(u16::try_from(n).unwrap())],
            })
            .collect()
    }

    /// The names of the trial scopes systemd holds right now, found anywhere in the cgroup tree.
    #[expect(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "systemd unit names are exact and case-sensitive: only `togi-trial-*.scope` is a trial scope"
    )]
    fn trial_scopes() -> Vec<String> {
        let mut names = Vec::new();
        let mut pending = vec![PathBuf::from(CGROUPS)];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if name.starts_with("togi-trial-") && name.ends_with(".scope") {
                    names.push(name);
                } else {
                    pending.push(entry.path());
                }
            }
        }
        names.sort();
        names
    }

    /// Waits until every expected trial scope is observable in the cgroup tree.
    fn await_scopes(count: usize) -> Vec<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let scopes = trial_scopes();
            if scopes.len() >= count {
                return scopes;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "only {scopes:?} of {count} trial scopes appeared"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn log(line: &str) {
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(path("smu.log"))
            .unwrap();
        writeln!(file, "pid={} {line}", std::process::id()).unwrap();
    }

    /// The fake SMU: every offset is a line in a file, so it survives the death of the process that wrote it.
    struct FileSmu;

    impl FileSmu {
        fn offsets() -> Vec<i64> {
            let stored: Vec<i64> = text("offsets")
                .split_whitespace()
                .filter_map(|value| value.parse().ok())
                .collect();
            if stored.len() == cores() {
                stored
            } else {
                vec![BASELINE; cores()]
            }
        }

        fn store(offsets: &[i64], operation: &str) {
            let staging = path("offsets.tmp");
            let line: Vec<String> = offsets.iter().map(i64::to_string).collect();
            std::fs::write(&staging, line.join(" ")).unwrap();
            std::fs::rename(&staging, path("offsets")).unwrap();
            log(&format!("{operation} {}", line.join(" ")));
        }
    }

    impl Smu for FileSmu {
        fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, Error> {
            let value = if text("fault") == "readback" && core.index() == 1 {
                -7
            } else {
                Self::offsets()[core.index()]
            };
            log(&format!("read core {:02} {value}", core.get()));
            Ok(ObservedOffset(value))
        }

        fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), Error> {
            let mut offsets = Self::offsets();
            offsets[core.index()] = offset.get();
            Self::store(&offsets, &format!("set core {:02}", core.get()));
            Ok(())
        }

        fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), Error> {
            Self::store(&vec![offset.get(); cores()], "set-all");
            Ok(())
        }

        fn emergency_zero(&mut self, _: SmuValidated, _: &Stopped) -> Result<(), Error> {
            let scopes = trial_scopes();
            let alive = if scopes.is_empty() {
                "none".to_owned()
            } else {
                scopes.join(",")
            };
            log(&format!("emergency-zero trial-scopes={alive}"));
            Self::store(&vec![0; cores()], "emergency-zero wrote");
            Ok(())
        }
    }

    struct VmHost;

    impl Host for VmHost {
        fn boot_id(&mut self) -> Result<BootId, Error> {
            togi_detect::boot_id().map_err(|error| other(error.to_string()))
        }

        fn topology(&mut self) -> Result<Vec<CoreInfo>, Error> {
            Ok(topology())
        }

        #[expect(
            clippy::disallowed_methods,
            reason = "the VM's fake SMU stands in for a validated driver unless the test fails the validation"
        )]
        fn validate_smu(&mut self) -> Result<SmuValidated, Error> {
            if text("fault") == "validation" {
                return Err(other("the SMU validation is refused by the test"));
            }
            Ok(SmuValidated::checked())
        }

        fn bios_context(&mut self) -> Result<BiosContext, Error> {
            Err(other("the VM has no BIOS context"))
        }

        fn ranking(&mut self) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> {
            Err(Partial {
                value: Vec::new(),
                error: other("the VM has no core ranking"),
            })
        }

        fn preflight(&mut self) -> Vec<Check> {
            Vec::new()
        }

        fn watchdog(&mut self) -> Check {
            Check::default()
        }
    }

    struct VmClock;

    impl Clock for VmClock {
        fn now(&self) -> Timestamp {
            Timestamp::now()
        }

        fn monotonic(&self) -> Result<Duration, togi_machine::Error> {
            let now = clock_gettime(ClockId::Monotonic);
            togi_machine::monotonic_duration(now.tv_sec, now.tv_nsec)
        }

        fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error> {
            if cancel.wait_timeout(duration) {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        }
    }

    struct VmKernel;

    impl Kernel for VmKernel {
        fn mces(&mut self, _: &BootId, _: Duration) -> Result<Vec<Mce>, Partial<Vec<Mce>>> {
            Err(Partial {
                value: Vec::new(),
                error: other("the VM has no kernel log reader"),
            })
        }

        fn read_mces(
            &mut self,
            _: &BootId,
            _: Option<&KernelCursor>,
        ) -> Result<KernelRead, Partial<KernelRead>> {
            Err(Partial {
                value: KernelRead::default(),
                error: other("the VM has no kernel log reader"),
            })
        }

        fn reset_reason(&mut self, _: &BootId) -> Result<ResetReason, Error> {
            Err(other("the VM has no kernel log reader"))
        }

        fn reset_reason_after(&mut self, _: &BootId) -> Result<ResetReason, Error> {
            Err(other("the VM has no kernel log reader"))
        }

        fn saved_pstore(&mut self, _: &BootId) -> Result<Option<PstoreRecord>, Error> {
            Err(other("the VM has no kernel log reader"))
        }
    }

    /// Day observation over the fake SMU, with the production kernel-log reader for successor evidence. A VM whose
    /// log cannot prove a successor reports it unavailable, which `togi day` records as an unknown reset reason.
    struct VmObserver {
        kernel: Option<togi_detect::Kernel>,
    }

    impl Observer for VmObserver {
        fn bios_context(&mut self) -> Result<BiosContext, Error> {
            Ok(BiosContext {
                bios_version: "vm".to_owned(),
                board: "vm".to_owned(),
                cpu_model: "vm".to_owned(),
                microcode: "vm".to_owned(),
                boost_limit_mhz: 0,
            })
        }

        fn offsets(&mut self) -> Result<PerCore<ObservedOffset>, Error> {
            let mut offsets = PerCore::default();
            for info in topology() {
                offsets.set(info.core, FileSmu.offset(info.core)?);
            }
            Ok(offsets)
        }

        fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
            match &mut self.kernel {
                Some(kernel) => kernel.reset_reason_after(boot),
                None => Err(other("the VM has no kernel log reader")),
            }
        }

        fn end_evidence(&mut self, boot: &BootId) -> togi_machine::EndEvidence {
            self.kernel
                .as_mut()
                .map_or_else(togi_machine::EndEvidence::unknown, |kernel| {
                    kernel.end_evidence(boot)
                })
        }

        fn watchdog(&mut self) -> togi_machine::WatchdogState {
            togi_machine::WatchdogState::Unarmed {
                detail: "the VM observer reads no watchdog".to_owned(),
            }
        }

        fn live_faults(
            &mut self,
            boot: &BootId,
            after: Option<&KernelCursor>,
        ) -> Result<KernelRead, Partial<KernelRead>> {
            match &mut self.kernel {
                Some(kernel) => kernel.read_mces(boot, after),
                None => Err(Partial {
                    value: KernelRead::default(),
                    error: other("the VM has no kernel log reader"),
                }),
            }
        }

        fn cpu_times(&mut self) -> Result<Vec<togi_machine::CpuTime>, Error> {
            match &self.kernel {
                Some(kernel) => Ok(kernel.read_cpu_times()?),
                None => Err(other("the VM has no kernel log reader")),
            }
        }
    }

    /// Writes a session's first event with the current build, as `togi run` would have, and nothing else. Only this
    /// helper does it: the production `togi day` never creates a session.
    fn seed_session() -> i32 {
        std::fs::create_dir_all(STATE_DIR).unwrap();
        let dir = StateDir::lock(STATE_DIR).unwrap();
        let now = Timestamp::now();
        let session = dir.session_id(now).unwrap();
        let boot = togi_detect::boot_id().unwrap();
        let mut journal = dir.open_at(boot, now).unwrap();
        let stamp = Stamp {
            time: now,
            mono: Some(VmClock.monotonic().unwrap()),
        };
        let start = SessionStart {
            build: togi_cli::report::current_build(),
            session,
            cores: topology(),
        };
        journal
            .append(stamp, Payload::SessionStart(start), &[])
            .unwrap();
        journal.close().unwrap();
        0
    }

    /// The shipped trial runner, with a sweep that fails when the test asks.
    struct VmTrials(Runner);

    impl Trials for VmTrials {
        fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
            if text("fault") == "sweep" {
                return Err(other("the trial-scope sweep is failed by the test"));
            }
            self.0.sweep(cancel)
        }

        fn start(
            &mut self,
            cancel: &CancellationToken,
            spec: TrialSpec,
            ready: Stopped,
        ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
            self.0.start(cancel, spec, ready)
        }

        fn ended(&mut self, id: &str) -> Result<(), Error> {
            self.0.ended(id)
        }

        fn samples<'a>(
            &'a self,
            id: &'a str,
        ) -> Box<dyn Iterator<Item = Result<TrialConditions, togi_machine::SampleError>> + 'a>
        {
            self.0.samples(id)
        }
    }

    /// A stress program that only sleeps: this executable's `workload` mode.
    struct Sleeper;

    impl Backend for Sleeper {
        fn kind(&self) -> togi_machine::Backend {
            togi_machine::Backend::Mprime
        }

        fn check(&self) -> Result<Ready, togi_backend::Error> {
            Ok(Ready {
                detail: "vm sleeper".to_owned(),
            })
        }

        fn plan(&self, _: &Workload, _: &Path, _: &[CpuId]) -> Result<Plan, togi_backend::Error> {
            Ok(Plan {
                program: std::env::current_exe().unwrap(),
                args: [
                    "--exact",
                    "vm_recovery::helper",
                    "--nocapture",
                    "--",
                    "workload",
                ]
                .map(OsString::from)
                .to_vec(),
                inputs: Vec::new(),
                watched: Vec::new(),
            })
        }

        fn classify(&self, _: &str) -> Line {
            Line::Other
        }
    }

    /// The host: [`Real`] in everything but its machine.
    #[derive(Default)]
    struct VmSystem(Real);

    impl System for VmSystem {
        fn check_platform(&self) -> Result<(), HardwareError> {
            self.0.check_platform()
        }

        fn topology(&self) -> Result<Vec<CoreInfo>, HardwareError> {
            self.0.topology()
        }

        fn privileged(&self) -> bool {
            self.0.privileged()
        }

        fn host_lock(&self) -> (&Path, Policy) {
            self.0.host_lock()
        }

        fn config_path(&self) -> &Path {
            self.0.config_path()
        }

        fn boot_id(&self) -> Result<BootId, togi_detect::Error> {
            self.0.boot_id()
        }

        fn now(&self) -> Timestamp {
            self.0.now()
        }

        fn monotonic(&self) -> Result<Duration, togi_machine::Error> {
            self.0.monotonic()
        }

        fn zone(&self) -> TimeZone {
            self.0.zone()
        }

        fn diagnose(
            &self,
            config: &Config,
            recorded: Option<&BiosContext>,
            lock: Option<&HostLock>,
        ) -> Result<Diagnosis, HardwareError> {
            self.0.diagnose(config, recorded, lock)
        }

        fn bootloader(&self, grubenv: &Path) -> io::Result<Box<dyn Bootloader>> {
            self.0.bootloader(grubenv)
        }

        fn create_retry_marker(&self) -> io::Result<()> {
            self.0.create_retry_marker()
        }

        fn remove_retry_marker(&self) -> io::Result<()> {
            self.0.remove_retry_marker()
        }

        fn reboot(&self) -> Result<(), RebootError> {
            self.0.reboot()
        }

        fn stderr_facts(&self) -> OutputFacts {
            self.0.stderr_facts()
        }

        fn open_machine<'lock>(
            &self,
            _: &Config,
            state_dir: &Path,
            _: &'lock HostLock,
        ) -> Result<Machine<'lock>, HardwareError> {
            let runner = Runner::new(Options {
                dir: state_dir.join("trials"),
                backends: Backends::new().with(Arc::new(Sleeper)),
                topology: topology(),
                identity: Some(BackendIdentity::lookup(ACCOUNT).unwrap()),
                pm_table: None,
            })
            .map_err(HardwareError::TrialRunner)?;
            Ok(Machine {
                clock: Box::new(VmClock),
                smu: Box::new(FileSmu),
                host: Box::new(VmHost),
                trials: Box::new(VmTrials(runner)),
                kernel: Box::new(VmKernel),
            })
        }

        fn day_observer<'lock>(
            &self,
            _: &'lock HostLock,
        ) -> Result<Box<dyn Observer + 'lock>, Error> {
            Ok(Box::new(VmObserver {
                kernel: togi_detect::Kernel::new(&topology()).ok(),
            }))
        }

        fn day_socket(
            &self,
        ) -> Result<Option<togi_cli::day::socket::Server>, togi_cli::day::socket::Error> {
            self.0.day_socket()
        }

        fn interactive(&self) -> bool {
            self.0.interactive()
        }

        fn stderr(&self) -> Box<dyn Write> {
            self.0.stderr()
        }

        fn surface(&self) -> Arc<dyn Surface> {
            self.0.surface()
        }
    }

    struct Quiet;

    impl Reporter for Quiet {
        fn progress(&mut self, _: &str) {}
        fn sample(&mut self, _: togi_machine::Sample) {}
        fn signal(&mut self, _: CoreId, _: Signal, _: Option<u32>, _: &str) {}
    }

    fn restore_baseline() {
        FileSmu::store(&vec![BASELINE; cores()], "restore-baseline");
    }

    /// What `togi run` does in the VM, as the `mode` file says. Offsets and trial scopes are real files and real
    /// systemd scopes, so killing this process leaves both behind as a crashed togi does.
    fn serve(system: &VmSystem) -> i32 {
        let mode = text("mode");
        let cancel = CancellationToken::new();
        let _listening = togi_cli::signals::listen(&cancel).unwrap();
        let (lock_path, policy) = system.host_lock();
        let lock = HostLock::acquire(lock_path, policy).unwrap();
        match mode.as_str() {
            "clean" => {
                restore_baseline();
                return 0;
            }
            "deadend" => {
                FileSmu::store(&trial_offsets(), "trial-offsets");
                restore_baseline();
                return 15;
            }
            "kill" | "fail" => {}
            unknown => panic!("unknown mode {unknown:?}"),
        }
        let mut machine = system
            .open_machine(&Config::default(), Path::new(STATE_DIR), &lock)
            .unwrap();
        FileSmu::store(&trial_offsets(), "trial-offsets");
        let mut offsets = PerCore::default();
        for info in topology() {
            let read = machine.smu.offset(info.core).unwrap();
            offsets.set(info.core, TargetOffset::new(read.0).unwrap());
        }
        let ready = machine.trials.sweep(&cancel).unwrap().stopped;
        let topology = topology();
        // R7 runs one instance per core, so the two loaded cores get one scope each.
        let regime = Regime::R7;
        let workload = pick_workload(regime, 0).id();
        let cores: Vec<CoreId> = topology[..LOADED].iter().map(|info| info.core).collect();
        let cpus: Vec<CpuId> = topology[..LOADED].iter().map(|info| info.cpus[0]).collect();
        let roles = TrialRole::legacy(regime, workload, 1, &cores, &cpus, &topology).unwrap();
        let spec = TrialSpec {
            id: format!("vm-{}-{}", Timestamp::now().as_second(), std::process::id()),
            regime,
            workload,
            condition: Condition::Together,
            cores,
            cpus,
            roles,
            duration: Duration::from_secs(3600),
            index: 0,
            seed: 1,
            offsets,
            controller: togi_machine::ControllerMode::Legacy,
        };
        let mut running = machine
            .trials
            .start(&cancel, spec, ready)
            .map_err(|partial| partial.error)
            .unwrap();
        let started = running.started();
        let scopes = await_scopes(LOADED);
        log(&format!(
            "trial started in {} scopes={}",
            started.scope,
            scopes.join(",")
        ));
        std::fs::write(path("trial"), &started.scope).unwrap();
        if mode == "fail" {
            // Exits without the runner's teardown, so the trial and the offsets outlive the process as in a crash.
            exit(1);
        }
        let _ = running.wait(&cancel, &mut Quiet);
        let _ = running.stop().unwrap();
        restore_baseline();
        0
    }

    fn run_togi(arguments: &[String]) -> i32 {
        let system = VmSystem::default();
        if arguments.get(1).map(String::as_str) == Some("seed-session") {
            return seed_session();
        }
        if arguments.get(1).map(String::as_str) == Some("run") {
            return serve(&system);
        }
        i32::from(togi_cli::run_process(arguments, &system).code())
    }

    /// The `helper` test is the executable's entry; `cargo test` runs it without the marker and it returns.
    #[test]
    fn helper() {
        let arguments: Vec<String> = std::env::args().collect();
        let Some(marker) = arguments.iter().position(|argument| argument == "--") else {
            return;
        };
        let rest = arguments[marker + 1..].to_vec();
        match rest.first().map(String::as_str) {
            Some("workload") => loop {
                std::thread::sleep(Duration::from_secs(3600));
            },
            Some("togi") => {
                let status = std::thread::Builder::new()
                    .stack_size(16 << 20)
                    .spawn(move || run_togi(&rest))
                    .unwrap()
                    .join()
                    .unwrap_or(101);
                exit(status);
            }
            _ => {}
        }
    }
}
