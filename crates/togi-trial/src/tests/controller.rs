//! The controller on the trial seams. The orchestration tests run on the fake OS with a recording
//! controller; the process seam test re-executes the test binary as a child that confines its own
//! threads, so the shared test process never changes its affinity.

use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use togi_machine::{
    CancellationToken, CcdId, CoreInfo, CpuId, Regime, TargetOffset, TrialSpec, Trials,
};

use super::harness::{
    core, crate_error, finish, local, nothing_runs, rig_edit, spec as base_spec, topology,
    with_roles,
};
use super::process::{Helper, allowed_cpus, linux, timing};
use crate::Error;
use crate::controller::{Call, Controller, Moved, Proc, Recording, Threads};
use crate::launch::cpus_list;
use crate::native::Settings;
use crate::os::Systemd;
use crate::os::fake::{Fake, Launch, ProcCtx, Reply, script};
use crate::runner::{Backends, Options, Runner};
use crate::runtime::{Command, serve_detached};
use crate::sensors::Roots;
use crate::supervisor::Env;
use crate::testing::TestBackend;
use crate::timing::Timing;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// Every logical CPU of the idle CCD, ascending.
fn idle_ccd(loaded: u8) -> Vec<CpuId> {
    let mut cpus: Vec<CpuId> = topology()
        .into_iter()
        .filter(|info| info.ccd.get() != loaded / 8)
        .flat_map(|info| info.cpus)
        .collect();
    cpus.sort_unstable_by_key(|cpu| cpu.get());
    cpus
}

fn recorded() -> (Arc<Recording>, impl FnOnce(&mut Env<Fake>)) {
    let recording = Arc::new(Recording::default());
    let handed = Arc::clone(&recording);
    (recording, move |env: &mut Env<Fake>| {
        env.controller = handed;
    })
}

#[test]
fn ordinary_supervision_requests_use_managed_placement() {
    assert_eq!(
        base_spec(Regime::R1, &[2], secs(5)).controller,
        togi_machine::ControllerMode::Managed {
            rotation: 0,
            purpose: togi_machine::Purpose::Isolated,
        }
    );
}

#[test]
fn the_controller_set_is_in_place_before_the_launch_and_follows_each_trials_loaded_core() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (recording, install) = recorded();
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), install);
            let early = Arc::new(AtomicBool::new(false));
            let (seen, calls) = (Arc::clone(&early), Arc::clone(&recording));
            rig.fake.on_launch(move |_, _| {
                seen.store(!calls.calls().is_empty(), Ordering::SeqCst);
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    ctx.until_killed().await;
                }))
            });
            let first = rig
                .start(base_spec(Regime::R1, &[2], secs(5)))
                .await
                .unwrap();
            assert!(
                early.load(Ordering::SeqCst),
                "the confinement came after the launch"
            );
            assert_eq!(recording.calls(), [Call::Confine(idle_ccd(2))]);
            let run = finish(&first).await;
            assert!(run.finished.teardown.is_ok());
            assert_eq!(
                recording.calls(),
                [Call::Confine(idle_ccd(2)), Call::Restore],
                "restored once the teardown confirmed"
            );
            assert!(
                run.progress()
                    .iter()
                    .any(|line| line.contains("their own CPUs again")),
                "{:?}",
                run.progress()
            );

            let second = rig
                .start(base_spec(Regime::R1, &[10], secs(5)))
                .await
                .unwrap();
            finish(&second).await;
            assert_eq!(
                recording.calls()[2..],
                [Call::Confine(idle_ccd(10)), Call::Restore]
            );
        })
        .await;
    });
}

#[test]
fn a_start_that_fails_to_launch_restores_after_its_confirmed_rollback() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (recording, install) = recorded();
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), install);
            rig.fake
                .on_launch(|_, _| Launch::Fail(io::Error::other("no process left")));
            let failed = rig
                .start(base_spec(Regime::R1, &[2], secs(5)))
                .await
                .map(|_| ())
                .unwrap_err();
            assert!(
                failed.value.is_some(),
                "the rollback confirmed nothing runs"
            );
            assert_eq!(
                recording.calls(),
                [Call::Confine(idle_ccd(2)), Call::Restore]
            );
            assert!(
                matches!(crate_error(&failed.error), Some(Error::Launch { .. })),
                "a complete restoration added to the launch error: {:?}",
                failed.error
            );
        })
        .await;
    });
}

/// A process that never exits and holds its pipes: nothing can confirm it stopped.
fn immortal() -> Launch {
    Launch::Run(script(|ctx: ProcCtx| async move {
        let _keep = ctx;
        std::future::pending::<()>().await;
    }))
}

fn kill_hangs(call: &Systemd) -> Option<Reply> {
    if let Systemd::Kill { .. } = call {
        Some(Reply::Hang)
    } else {
        None
    }
}

#[test]
fn a_teardown_that_cannot_confirm_leaves_the_threads_confined() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (recording, install) = recorded();
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), install);
            rig.fake.on_launch(|_, _| immortal());
            rig.fake.on_systemd(kill_hangs);
            let cancel = CancellationToken::new();
            let handle = rig
                .start_with(&cancel, base_spec(Regime::R1, &[2], secs(60)))
                .await
                .unwrap();
            cancel.cancel();
            let run = finish(&handle).await;
            assert!(run.finished.teardown.is_err(), "the teardown confirmed");
            assert_eq!(
                recording.calls(),
                [Call::Confine(idle_ccd(2))],
                "restored although nothing proved the workload stopped"
            );
        })
        .await;
    });
}

#[test]
fn a_rollback_that_cannot_confirm_leaves_the_threads_confined() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (recording, install) = recorded();
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), install);
            rig.fake.on_launch(|index, _| {
                if index == 0 {
                    immortal()
                } else {
                    Launch::Fail(io::Error::other("no process left"))
                }
            });
            rig.fake.on_systemd(kill_hangs);
            let failed = rig
                .start(base_spec(Regime::R7, &[0, 1], secs(60)))
                .await
                .map(|_| ())
                .unwrap_err();
            assert!(failed.value.is_none(), "the rollback cannot have confirmed");
            assert!(
                matches!(recording.calls()[..], [Call::Confine(_)]),
                "{:?}",
                recording.calls()
            );
        })
        .await;
    });
}

#[test]
fn a_sweep_that_proves_nothing_runs_restores_threads_a_start_left_confined() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (recording, install) = recorded();
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), install);
            let (reply, answered) = std::sync::mpsc::sync_channel(1);
            serve_detached(
                Rc::clone(&rig.env),
                Command::Sweep {
                    cancel: CancellationToken::new(),
                    reply,
                },
            )
            .await;
            let swept = answered.recv().unwrap().unwrap();
            assert_eq!(recording.calls(), [Call::Restore]);
            assert!(
                swept.detail.contains("their own CPUs again"),
                "{}",
                swept.detail
            );
        })
        .await;
    });
}

// ---- incomplete moves ----

/// What a move reports when a thread could not be read or moved.
const FAILED: Moved = Moved {
    threads: 9,
    failed: 2,
    incomplete: false,
};

/// What a move reports when the pass limit was reached while threads kept appearing.
const BOUNDED: Moved = Moved {
    threads: 9,
    failed: 0,
    incomplete: true,
};

/// Records its calls as [`Recording`] does, and reports `confined` for every confinement and
/// `restored` for every restoration.
struct Reporting {
    recording: Recording,
    confined: Moved,
    restored: Moved,
}

impl Controller for Reporting {
    fn confine(&self, cpus: &[CpuId]) -> Option<Moved> {
        self.recording.confine(cpus).map(|_| self.confined)
    }

    fn restore(&self) -> Option<Moved> {
        self.recording.restore().map(|_| self.restored)
    }
}

fn reporting(confined: Moved, restored: Moved) -> (Arc<Reporting>, impl FnOnce(&mut Env<Fake>)) {
    let controller = Arc::new(Reporting {
        recording: Recording::default(),
        confined,
        restored,
    });
    let handed = Arc::clone(&controller);
    (controller, move |env: &mut Env<Fake>| {
        env.controller = handed;
    })
}

fn retry_line(counts: &str) -> String {
    format!(
        "togi's own threads were not all given their own CPUs back and stay confined until a \
         later confirmed teardown or sweep retries: {counts}"
    )
}

#[test_case::test_case(FAILED, "9 threads, 2 could not be read or moved"; "failed")]
#[test_case::test_case(BOUNDED, "9 threads, new threads were still appearing when the pass limit was reached"; "bounded")]
fn an_incomplete_confinement_and_restoration_warn_in_the_trials_progress(
    moved: Moved,
    counts: &str,
) {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (controller, install) = reporting(moved, moved);
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), install);
            rig.fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    ctx.until_killed().await;
                }))
            });
            let spec = base_spec(Regime::R1, &[2], secs(5));
            let loaded = cpus_list(&spec.cpus);
            let handle = rig.start(spec).await.unwrap();
            let run = finish(&handle).await;
            assert!(run.finished.teardown.is_ok(), "{moved:?}");
            assert_eq!(
                controller.recording.calls(),
                [Call::Confine(idle_ccd(2)), Call::Restore]
            );
            let confined = format!(
                "togi controller CPUs {} while the trial loads {loaded}; idle CO-0 CCD 1, capped; {counts}",
                cpus_list(&idle_ccd(2))
            );
            let restored = retry_line(counts);
            let reports: Vec<&str> = run
                .progress()
                .into_iter()
                .filter(|line| {
                    line.starts_with("togi controller CPUs") || line.starts_with("togi's own threads")
                })
                .collect();
            assert_eq!(reports, [confined.as_str(), restored.as_str()]);
            assert!(
                run.finished
                    .result
                    .inconclusive
                    .as_ref()
                    .is_some_and(|reason| reason.contains("did not confine every controller thread"))
            );
        })
        .await;
    });
}

#[test_case::test_case(FAILED, "9 threads, 2 could not be read or moved"; "failed")]
#[test_case::test_case(BOUNDED, "9 threads, new threads were still appearing when the pass limit was reached"; "bounded")]
fn a_start_that_fails_to_launch_returns_an_incomplete_restoration_with_its_error(
    moved: Moved,
    counts: &str,
) {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (controller, install) = reporting(Moved::default(), moved);
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), install);
            rig.fake
                .on_launch(|_, _| Launch::Fail(io::Error::other("no process left")));
            let failed = rig
                .start(base_spec(Regime::R1, &[2], secs(5)))
                .await
                .map(|_| ())
                .unwrap_err();
            assert!(
                failed.value.is_some(),
                "the rollback confirmed nothing runs"
            );
            assert_eq!(
                controller.recording.calls(),
                [Call::Confine(idle_ccd(2)), Call::Restore]
            );
            let Some(Error::Joined(joined)) = crate_error(&failed.error) else {
                panic!("the restoration was not joined: {:?}", failed.error);
            };
            let restored = retry_line(counts);
            assert!(
                matches!(joined.errors(), [Error::Launch { .. }, Error::Output(output)]
                if output.diagnostics() == [restored]),
                "{:?}",
                joined.errors()
            );
        })
        .await;
    });
}

#[test_case::test_case(FAILED, "9 threads, 2 could not be read or moved"; "failed")]
#[test_case::test_case(BOUNDED, "9 threads, new threads were still appearing when the pass limit was reached"; "bounded")]
fn a_sweep_whose_restoration_is_incomplete_says_it_stays_confined_until_a_retry(
    moved: Moved,
    counts: &str,
) {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (controller, install) = reporting(Moved::default(), moved);
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), install);
            let (reply, answered) = std::sync::mpsc::sync_channel(1);
            serve_detached(
                Rc::clone(&rig.env),
                Command::Sweep {
                    cancel: CancellationToken::new(),
                    reply,
                },
            )
            .await;
            let swept = answered.recv().unwrap().unwrap();
            assert_eq!(controller.recording.calls(), [Call::Restore]);
            assert!(
                swept.detail.ends_with(&format!("; {}", retry_line(counts))),
                "{}",
                swept.detail
            );
            assert!(
                !swept.detail.contains("their own CPUs again"),
                "{}",
                swept.detail
            );
        })
        .await;
    });
}

// ---- the process seam ----

const CHILD: &str = "tests::controller::real_process_seam_child";
const MARK: &str = "helper:controller-seam";

/// The CPUs a `Cpus_allowed_list` value names, ascending.
fn cpu_list(text: &str) -> Vec<u16> {
    text.trim()
        .split(',')
        .filter(|part| !part.is_empty())
        .flat_map(|part| {
            let (from, to) = part.split_once('-').unwrap_or((part, part));
            from.parse::<u16>().unwrap()..=to.parse::<u16>().unwrap()
        })
        .collect()
}

fn allowed_in(status: &str) -> Option<Vec<u16>> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .map(cpu_list)
}

/// What every thread of the calling process may run on, by thread id; a thread that ended while
/// the directory was read is left out.
fn masks() -> BTreeMap<u32, Vec<u16>> {
    let mut found = BTreeMap::new();
    for entry in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let Some(tid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        else {
            continue;
        };
        if let Some(list) = std::fs::read_to_string(entry.path().join("status"))
            .ok()
            .and_then(|status| allowed_in(&status))
        {
            found.insert(tid, list);
        }
    }
    found
}

fn comm(tid: u32) -> String {
    std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

type Job = Box<dyn FnOnce() + Send>;

/// A thread that runs the jobs it is sent, so a test can have an existing thread create another.
fn worker() -> Sender<Job> {
    let (jobs, inbox) = channel::<Job>();
    std::thread::spawn(move || {
        while let Ok(job) = inbox.recv() {
            job();
        }
    });
    jobs
}

fn sleeper() {
    std::thread::spawn(|| std::thread::sleep(secs(120)));
}

/// Has `worker` create a thread, and waits until it did.
fn create_through(worker: &Sender<Job>) {
    worker.send(Box::new(sleeper)).unwrap();
    let (done, answered) = channel();
    worker
        .send(Box::new(move || done.send(()).unwrap()))
        .unwrap();
    answered.recv_timeout(secs(10)).unwrap();
}

fn trial_spec(id: &str, loaded: usize, topology: &[CoreInfo]) -> TrialSpec {
    let mut spec = base_spec(Regime::R1, &[0], secs(60));
    spec.id = id.into();
    spec.cores = vec![core(u8::try_from(loaded).unwrap())];
    spec.cpus = vec![topology[loaded].cpus[0]];
    spec.offsets = topology
        .iter()
        .map(|info| (info.core, TargetOffset::ZERO))
        .collect();
    with_roles(spec, topology)
}

/// Every thread of the process runs on exactly `expected`, none on a loaded CPU, and the runtime
/// thread and the threads made since the baseline are among them.
fn assert_confined(expected: &[u16], loaded: &[u16], at_least: usize) {
    let now = masks();
    assert!(now.len() >= at_least, "{} threads: {now:?}", now.len());
    for (tid, mask) in &now {
        assert_eq!(mask, expected, "thread {tid} ({})", comm(*tid));
        assert!(loaded.iter().all(|cpu| !mask.contains(cpu)), "thread {tid}");
    }
    assert!(
        now.keys().any(|tid| comm(*tid) == "togi-trial"),
        "the runtime thread is not among {now:?}"
    );
}

/// The workload keeps the CPUs it was given although the thread that launched it is confined to
/// others.
fn assert_workload_keeps(pid: u32, wanted: &[u16]) {
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).unwrap();
    for entry in tasks.flatten() {
        let status = std::fs::read_to_string(entry.path().join("status")).unwrap();
        assert_eq!(
            allowed_in(&status).unwrap(),
            wanted,
            "workload task {entry:?}"
        );
    }
}

fn seam() {
    let allowed = allowed_cpus();
    if allowed.len() < 2 {
        eprintln!("controller seam: fewer than two CPUs, so no core can stay unloaded");
        return;
    }
    // Core 0 is the first one or two allowed CPUs and core 1 the next, so a loaded core's SMT
    // sibling is on the list of CPUs to avoid whenever the machine has three CPUs.
    let (first, second) = if allowed.len() >= 3 {
        (allowed[..2].to_vec(), allowed[2..3].to_vec())
    } else {
        (vec![allowed[0]], vec![allowed[1]])
    };
    let topology: Vec<CoreInfo> = [first.clone(), second.clone()]
        .into_iter()
        .enumerate()
        .map(|(n, cpus)| CoreInfo {
            core: core(u8::try_from(n).unwrap()),
            ccd: CcdId::new(0).unwrap(),
            cpus: cpus.into_iter().map(CpuId::from).collect(),
        })
        .collect();

    let before: Vec<Sender<Job>> = (0..3).map(|_| worker()).collect();
    let baseline = masks();
    for (tid, mask) in &baseline {
        assert_eq!(mask, &allowed, "thread {tid} began confined");
    }

    let dir = crate::testing::tempdir();
    let options = Options {
        dir: dir.path().join("trials"),
        backends: Backends::new().with(Arc::new(Helper::new("hold", false))),
        topology: topology.clone(),
        identity: None,
        pm_table: None,
    };
    let settings = Settings {
        timing: timing(),
        roots: Roots::default(),
        fs: None,
        inputs: crate::procfs::Inputs::default(),
        memory: None,
        clock: crate::clock::Clock::System,
        durability: crate::samples::Durability::Unsynced,
        unscoped: true,
        user_manager: false,
        controller: Arc::new(Threads::over(Proc)),
        frequency: super::harness::frequency(&topology),
    };
    let mut runner = Runner::build(options, settings, linux).unwrap();

    for (id, loaded, expected) in [("t1", 0, &second), ("t2", 1, &first)] {
        let loaded_cpus: Vec<u16> = topology[loaded].cpus.iter().map(|cpu| cpu.get()).collect();
        let spec = trial_spec(id, loaded, &topology);
        let workload = spec.cpus[0].get();
        let trial = runner
            .start(&CancellationToken::new(), spec, nothing_runs())
            .map_err(|partial| partial.error)
            .unwrap();
        // Threads created while the trial runs: by this thread, and by a thread that existed
        // before the trial.
        sleeper();
        create_through(&before[0]);
        let floor = baseline.len() + 3;
        assert_confined(expected, &loaded_cpus, floor);
        assert_workload_keeps(trial.started().pid, &[workload]);
        assert!(trial.stop().is_ok(), "{id}: the stop was not confirmed");
        for (tid, mask) in &masks() {
            let wanted = baseline.get(tid).unwrap_or(&allowed);
            assert_eq!(
                mask,
                wanted,
                "{id}: thread {tid} ({}) was not restored",
                comm(*tid)
            );
        }
    }
}

/// Runs only as the child of the test below.
#[test]
fn real_process_seam_child() {
    if !std::env::args().any(|arg| arg == MARK) {
        return;
    }
    seam();
    std::process::exit(0);
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the process seam test re-executes the test binary so its affinity changes stay in the child"
)]
fn real_process_every_thread_leaves_the_loaded_core_during_a_trial_and_returns_after_it() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture", "--test-threads=1", MARK])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
