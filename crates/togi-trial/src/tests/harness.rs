//! A trial runner over the fake OS under paused time.

use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use togi_machine::{
    CancellationToken, CcdId, Condition, CoreId, CoreInfo, CpuId, Partial, Regime, Stopped,
    TrialSpec, pick_workload,
};
use tokio::task::LocalSet;

use crate::identity::BackendIdentity;
use crate::os::fake::Fake;
use crate::runner::Backends;
use crate::sensors::Roots;
use crate::supervisor::{Env, Event, Finished, Handle, start};
use crate::testing::TestBackend;
use crate::timing::Timing;

pub(super) fn core(n: u8) -> CoreId {
    CoreId::new(i64::from(n)).unwrap()
}

/// Cores 0..16; core n sits on CCD n/8 with logical CPUs n and n + 32.
pub(super) fn topology() -> Vec<CoreInfo> {
    (0..16)
        .map(|n| CoreInfo {
            core: core(n),
            ccd: CcdId(n / 8),
            cpus: vec![CpuId(u16::from(n)), CpuId(u16::from(n) + 32)],
        })
        .collect()
}

pub(super) fn spec(regime: Regime, cores: &[u8], duration: Duration) -> TrialSpec {
    let topology = topology();
    let cpus = if regime.instance_per_core() {
        cores
            .iter()
            .map(|&n| topology[usize::from(n)].cpus[0])
            .collect()
    } else if regime == Regime::R5 {
        topology[usize::from(cores[0])].cpus.clone()
    } else {
        vec![topology[usize::from(cores[0])].cpus[0]]
    };
    TrialSpec {
        id: "t1".into(),
        regime,
        workload: pick_workload(regime, 0).id(),
        condition: Condition::Alone,
        cores: cores.iter().map(|&n| core(n)).collect(),
        cpus,
        duration,
        index: 0,
        seed: 7,
    }
}

pub(super) struct Rig {
    pub(super) fake: Fake,
    pub(super) env: Rc<Env<Fake>>,
    pub(super) dir: TempDir,
}

/// The account of the fixtures that need a backend identity: the effective user and group of the
/// test process, so a root-owned source tree is irrelevant. Root and the reserved ids cannot be a
/// backend account, so a root run uses the conventional `nobody` (65534); the fixtures' directories
/// are world-searchable, so that account can reach them.
pub(crate) fn own_identity() -> BackendIdentity {
    identity_of(
        rustix::process::geteuid().as_raw(),
        rustix::process::getegid().as_raw(),
    )
}

pub(super) fn identity_of(uid: u32, gid: u32) -> BackendIdentity {
    const NOBODY: u32 = 65534;
    BackendIdentity::new(uid, gid)
        .or_else(|_| BackendIdentity::new(NOBODY, NOBODY))
        .unwrap()
}

pub(super) fn rig_with(backend: TestBackend, timing: Timing) -> Rig {
    rig_edit(backend, timing, |_| {})
}

pub(super) fn rig_edit(
    backend: TestBackend,
    timing: Timing,
    edit: impl FnOnce(&mut Env<Fake>),
) -> Rig {
    let dir = crate::testing::tempdir();
    let fake = Fake::default();
    let roots = Roots {
        hwmon: dir.path().join("hwmon"),
        cpu: dir.path().join("cpu"),
        powercap: dir.path().join("powercap"),
        proc: dir.path().join("proc"),
    };
    let mut env = Env {
        os: Rc::new(fake.clone()),
        timing,
        roots,
        dir: dir.path().join("trials"),
        backends: Backends::new().with(Arc::new(backend)),
        topology: topology(),
        identity: Some(own_identity()),
        unscoped: false,
        user_manager: false,
        pm_table: None,
        storage: None,
        log_sink: None,
        supervisors: std::cell::RefCell::default(),
        preparer: crate::prepare::Preparer::default(),
    };
    edit(&mut env);
    let env = Rc::new(env);
    Rig { fake, env, dir }
}

pub(super) fn rig() -> Rig {
    rig_with(TestBackend::new("/bin/true"), Timing::default())
}

/// A proof that nothing runs, for tests; the production crate mints it only after a teardown.
pub(super) fn nothing_runs() -> Stopped {
    #[expect(
        clippy::disallowed_methods,
        reason = "a test stands in for the session's proof that nothing runs"
    )]
    Stopped::confirmed()
}

impl Rig {
    pub(super) async fn start(&self, spec: TrialSpec) -> Result<Handle, Partial<Option<Stopped>>> {
        start(
            Rc::clone(&self.env),
            CancellationToken::new(),
            spec,
            nothing_runs(),
        )
        .await
    }

    pub(super) async fn start_with(
        &self,
        cancel: &CancellationToken,
        spec: TrialSpec,
    ) -> Result<Handle, Partial<Option<Stopped>>> {
        start(Rc::clone(&self.env), cancel.clone(), spec, nothing_runs()).await
    }
}

/// What a trial reported on its way to the end.
pub(super) struct Run {
    pub(super) events: Vec<Event>,
    pub(super) finished: Finished,
}

impl Run {
    pub(super) fn progress(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Progress(detail) => Some(detail.as_str()),
                _ => None,
            })
            .collect()
    }

    pub(super) fn signals(&self) -> Vec<(CoreId, togi_machine::Signal)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Signal { core, signal, .. } => Some((*core, *signal)),
                _ => None,
            })
            .collect()
    }

    pub(super) fn warnings(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Sample(sample) => Some(sample.warning.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Lets the supervisor run, in virtual time, until it reports its end.
pub(super) async fn finish(handle: &Handle) -> Run {
    let mut events = Vec::new();
    loop {
        match handle.events.try_recv() {
            Ok(Event::Finished(finished)) => {
                return Run {
                    events,
                    finished: *finished,
                };
            }
            Ok(event) => events.push(event),
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
}

/// The samples the writer thread has appended to a real file. The writer runs in real time, and
/// under a paused clock the ticks come microseconds apart, so a test that counts samples on disk
/// holds each tick until the previous sample has landed; the supervisor itself never waits.
#[derive(Default)]
pub(super) struct Landed {
    count: std::sync::Mutex<usize>,
    appended: std::sync::Condvar,
}

struct Landing {
    file: std::fs::File,
    landed: Arc<Landed>,
}

impl crate::samples::Storage for Landing {
    fn append(&mut self, line: &[u8]) -> std::io::Result<()> {
        std::io::Write::write_all(&mut self.file, line)?;
        self.file.sync_all()?;
        *self.landed.count.lock().unwrap() += 1;
        self.landed.appended.notify_all();
        Ok(())
    }

    fn close(self: Box<Self>) -> std::io::Result<()> {
        self.file.sync_all()
    }
}

impl Landed {
    /// Storage that creates `path` and counts each appended line.
    pub(super) fn storage(
        self: &Arc<Self>,
        path: std::path::PathBuf,
    ) -> Rc<dyn Fn() -> Result<crate::samples::Open, crate::error::StorageError>> {
        let landed = Arc::clone(self);
        Rc::new(move || {
            let (path, landed) = (path.clone(), Arc::clone(&landed));
            Ok(Box::new(move || {
                let file =
                    std::fs::File::create_new(path).map_err(crate::error::StorageError::Create)?;
                Ok(Box::new(Landing { file, landed }) as Box<dyn crate::samples::Storage>)
            }) as crate::samples::Open)
        })
    }

    /// Blocks the runtime thread, and with it the paused clock, until `lines` have landed.
    pub(super) fn wait_for(&self, lines: usize) {
        let count = self.count.lock().unwrap();
        let (_count, waited) = self
            .appended
            .wait_timeout_while(count, Duration::from_secs(60), |count| *count < lines)
            .unwrap();
        assert!(
            !waited.timed_out(),
            "the writer never appended {lines} samples"
        );
    }
}

pub(super) async fn local<F: Future>(test: F) -> F::Output {
    LocalSet::new().run_until(test).await
}

use crate::os::fake::{Launch, ProcCtx, ProcShared, script};
use crate::procfs::Usage;

/// A process that prints `lines` on stdout once, then runs until killed.
pub(super) fn printing(
    lines: &'static str,
) -> impl FnMut(usize, &crate::launch::LaunchCommand) -> Launch {
    move |_, _| {
        Launch::Run(script(move |mut ctx: ProcCtx| async move {
            ctx.print(lines).await;
            ctx.until_killed().await;
        }))
    }
}

/// Makes the process burn `cpus` CPUs worth of CPU time, reported every second.
pub(super) fn burn(shared: &std::rc::Rc<ProcShared>, cpus: u32) {
    let shared = std::rc::Rc::clone(shared);
    tokio::task::spawn_local(async move {
        let mut total = Duration::ZERO;
        while !shared.exited() {
            tokio::time::sleep(Duration::from_secs(1)).await;
            total += Duration::from_secs(1) * cpus;
            *shared.usage.borrow_mut() = Ok(Usage {
                cpu_time: total,
                cpu: 0,
            });
        }
    });
}

/// When a worker printed, and the instance it was.
pub(super) type Ticks = Rc<std::cell::RefCell<Vec<(usize, tokio::time::Instant)>>>;

/// A worker that produces output on its own every `period`, but only while it is not stopped:
/// `SIGSTOP` silences it until `SIGCONT`, as it does a real process. Each line is recorded in
/// `ticks` with the instant it was written, so a test can prove the quiet intervals.
pub(super) fn ticking(
    period: Duration,
    ticks: &Ticks,
) -> impl FnMut(usize, &crate::launch::LaunchCommand) -> Launch + use<> {
    use crate::os::fake::Delivery;
    use crate::os::{ExitInfo, GroupSignal};
    let ticks = Rc::clone(ticks);
    move |index, _| {
        let ticks = Rc::clone(&ticks);
        Launch::Run(script(move |mut ctx: ProcCtx| async move {
            let mut stopped = false;
            let mut clock = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            loop {
                tokio::select! {
                    delivery = ctx.signals.recv() => match delivery {
                        Some(Delivery::Group(GroupSignal::Stop)) => stopped = true,
                        Some(Delivery::Group(GroupSignal::Cont)) => stopped = false,
                        Some(Delivery::Group(GroupSignal::Term | GroupSignal::Kill) | Delivery::Scope(_)) => {
                            ctx.exit(ExitInfo::Signal(9));
                            return;
                        }
                        None => return,
                    },
                    _ = clock.tick() => {
                        if !stopped {
                            ticks.borrow_mut().push((index, tokio::time::Instant::now()));
                            ctx.print("ok tick\n").await;
                        }
                    }
                }
            }
        }))
    }
}

/// Whether a write at `at` can have come from a process that was running: it was, strictly before
/// `at` or counting the signals delivered at `at` itself. A write in the same virtual instant as a
/// stop or a continue may land on either side of it, so only a write strictly inside a stopped
/// interval is a violation.
pub(super) fn running_at(
    signals: &[(tokio::time::Instant, crate::os::GroupSignal)],
    at: tokio::time::Instant,
) -> bool {
    use crate::os::GroupSignal;
    let state = |inclusive: bool| {
        let mut running = true;
        for (when, signal) in signals {
            if *when > at || (!inclusive && *when == at) {
                break;
            }
            match signal {
                GroupSignal::Stop => running = false,
                GroupSignal::Cont => running = true,
                GroupSignal::Term | GroupSignal::Kill => return true,
            }
        }
        running
    };
    state(false) || state(true)
}

mod tests {
    use tokio::time::Instant;

    use super::*;
    use crate::os::fake::SignalErrorKind;
    use crate::os::{GroupSignal, KillSignal, Os, Systemd};

    #[test]
    fn the_fixture_identity_is_the_effective_account_or_nobody_for_root() {
        assert_eq!(
            (identity_of(1000, 100).uid(), identity_of(1000, 100).gid()),
            (1000, 100)
        );
        for (uid, gid) in [(0, 0), (0, 100), (1000, 0), (u32::MAX, 100)] {
            let identity = identity_of(uid, gid);
            assert_eq!((identity.uid(), identity.gid()), (65534, 65534));
        }
        let own = own_identity();
        let (uid, gid) = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        if uid != 0 && gid != 0 {
            assert_eq!((own.uid(), own.gid()), (uid, gid));
        } else {
            assert_eq!((own.uid(), own.gid()), (65534, 65534));
        }
    }

    fn launch_under(
        fake: &Fake,
        identity: BackendIdentity,
        cwd: &std::path::Path,
    ) -> Rc<std::cell::RefCell<Option<std::io::Result<()>>>> {
        use crate::os::Os as _;
        let argv = crate::launch::scope_argv(
            "togi-trial-t1-c00.scope",
            identity,
            false,
            cwd,
            &[CpuId(0)],
            &["worker".into()],
        );
        let target = cwd.join("results.txt");
        let written = Rc::new(std::cell::RefCell::new(None));
        let slot = Rc::clone(&written);
        fake.on_launch(move |_, _| {
            let (target, slot) = (target.clone(), Rc::clone(&slot));
            Launch::Run(script(move |ctx: ProcCtx| async move {
                *slot.borrow_mut() = Some(ctx.write_file(target, "COMPUTE ERROR\n"));
            }))
        });
        let command = crate::launch::LaunchCommand {
            argv,
            cwd: cwd.to_owned(),
            pin: Vec::new(),
        };
        fake.launch(&command).unwrap_or_else(|_| panic!("launch"));
        written
    }

    #[tokio::test(start_paused = true)]
    async fn a_fake_process_takes_the_account_its_scope_runs_it_under() {
        local(async {
            let fake = Fake::default();
            let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
            let slot = Rc::clone(&seen);
            fake.on_launch(move |_, _| {
                let slot = Rc::clone(&slot);
                Launch::Run(script(move |ctx: ProcCtx| async move {
                    slot.borrow_mut().push(ctx.account);
                }))
            });
            let identity = BackendIdentity::new(997, 996).unwrap();
            let scoped = crate::launch::LaunchCommand {
                argv: crate::launch::scope_argv(
                    "togi-trial-t1-c00.scope",
                    identity,
                    false,
                    std::path::Path::new("/trials/t1/c00"),
                    &[CpuId(0)],
                    &["worker".into()],
                ),
                cwd: "/trials/t1/c00".into(),
                pin: Vec::new(),
            };
            let bare = crate::launch::LaunchCommand {
                argv: vec!["worker".into()],
                ..scoped.clone()
            };
            for command in [&scoped, &bare] {
                let _ = crate::os::Os::launch(&fake, command);
            }
            tokio::task::yield_now().await;
            assert_eq!(*seen.borrow(), [Some((997, 996)), None]);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn every_watched_write_of_a_rig_process_targets_the_rigs_configured_account() {
        local(async {
            let rig = rig();
            let seen = Rc::new(std::cell::Cell::new(None));
            let slot = Rc::clone(&seen);
            rig.fake.on_launch(move |_, _| {
                let slot = Rc::clone(&slot);
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    slot.set(Some(ctx.account));
                    ctx.until_killed().await;
                }))
            });
            let handle = rig
                .start(spec(Regime::R1, &[0], Duration::from_secs(5)))
                .await
                .unwrap();
            tokio::task::yield_now().await;
            let identity = rig.env.identity.unwrap();
            assert_eq!(seen.get(), Some(Some((identity.uid(), identity.gid()))));
            drop(handle);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_privileged_run_hands_watched_files_to_the_configured_account() {
        if !rustix::process::geteuid().is_root() {
            eprintln!("skipped: needs root to chown a watched file to another account");
            return;
        }
        local(async {
            let dir = crate::testing::tempdir();
            let identity = identity_of(0, 0);
            let fake = Fake::default();
            let written = launch_under(&fake, identity, dir.path());
            tokio::task::yield_now().await;
            assert!(matches!(written.borrow().as_ref(), Some(Ok(()))));
            let meta = std::fs::metadata(dir.path().join("results.txt")).unwrap();
            let owner = std::os::unix::fs::MetadataExt::uid(&meta);
            assert_eq!(owner, identity.uid());
            assert_ne!(owner, 0);
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_expired_systemd_deadline_times_out_without_running_the_script_or_its_effect() {
        let fake = Fake::default();
        let scripted = Rc::new(std::cell::Cell::new(0));
        let seen = Rc::clone(&scripted);
        fake.on_systemd(move |_| {
            seen.set(seen.get() + 1);
            None
        });
        fake.add_unit("togi-trial-t1.scope");
        let scope = crate::id::ScopeName::from_unit("togi-trial-t1.scope").unwrap();
        let now = Instant::now();
        for deadline in [now, now - Duration::from_millis(1)] {
            for operation in [
                Systemd::Stop {
                    scope: scope.clone(),
                },
                Systemd::Kill {
                    scope: scope.clone(),
                    signal: KillSignal::Kill,
                },
                Systemd::ListScopes,
            ] {
                let outcome = fake.systemd(&operation, deadline).await;
                assert!(
                    matches!(outcome, Err(togi_command::Error::TimedOut { .. })),
                    "{operation:?}"
                );
            }
        }
        assert_eq!(scripted.get(), 0, "the script is never asked");
        assert_eq!(
            fake.units(),
            ["togi-trial-t1.scope"],
            "the stop was not applied"
        );
        let later = fake
            .systemd(
                &Systemd::Stop { scope },
                Instant::now() + Duration::from_secs(1),
            )
            .await;
        assert!(later.is_ok(), "a live deadline still applies the effect");
        assert!(fake.units().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_one_shot_signal_failure_fails_only_the_next_matching_delivery() {
        use crate::os::Child as _;
        let fake = Fake::default();
        local(async {
            let command = crate::launch::LaunchCommand {
                argv: vec!["worker".into()],
                cwd: std::path::PathBuf::from("."),
                pin: Vec::new(),
            };
            let child = fake.launch(&command).unwrap_or_else(|_| panic!("launch"));
            fake.fail_signal_once(GroupSignal::Stop, SignalErrorKind::Failed);
            assert!(child.signal_group(GroupSignal::Cont).is_ok());
            assert!(child.signal_group(GroupSignal::Stop).is_err());
            assert!(child.signal_group(GroupSignal::Stop).is_ok());
            assert_eq!(
                fake.process(0).signals.borrow()[..],
                [GroupSignal::Cont, GroupSignal::Stop, GroupSignal::Stop]
            );
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_threads_hook_runs_before_the_read_and_can_change_what_it_returns() {
        let fake = Fake::default();
        local(async {
            let command = crate::launch::LaunchCommand {
                argv: vec!["worker".into()],
                cwd: std::path::PathBuf::from("."),
                pin: Vec::new(),
            };
            let child = fake.launch(&command).unwrap_or_else(|_| panic!("launch"));
            let pid = crate::os::Child::pid(&child);
            assert!(fake.threads(pid).is_ok());
            let process = fake.process(0);
            let seen = Rc::new(std::cell::Cell::new(None));
            let recorded = Rc::clone(&seen);
            fake.on_threads(move |pid| {
                recorded.set(Some(pid));
                *process.threads.borrow_mut() =
                    Err(crate::procfs::ProcError::Observation("lost".to_owned()));
            });
            assert!(matches!(
                fake.threads(pid),
                Err(crate::procfs::ProcError::Observation(_))
            ));
            assert_eq!(seen.get(), Some(pid));
        })
        .await;
    }
}

/// The crate's own error inside a seam error, when it carries one.
pub(super) fn crate_error(error: &togi_machine::Error) -> Option<&crate::Error> {
    match error {
        togi_machine::Error::Other(inner) => inner.downcast_ref::<crate::Error>(),
        _ => None,
    }
}
