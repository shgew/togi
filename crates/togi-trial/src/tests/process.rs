//! Real processes through the private unscoped path: the test binary re-executes itself as a
//! helper, so no host scope, root or systemd is involved. None of this is containment.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use togi_backend::{Backend, Error, FileName, Line, Plan, Ready};
use togi_machine::{CancellationToken, CpuId, Regime, Signal, Stopped, Trials, Workload};

use super::harness::{core, crate_error, nothing_runs, topology as _topology};
use crate::os::linux::Linux;
use crate::procfs::{Proc, ticks_per_second};
use crate::runner::{Backends, Options, Runner, Settings};
use crate::sensors::Roots;
use crate::timing::Timing;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

const HELPER: &str = "tests::process::helper";

/// The mode a helper process runs, read from its own arguments.
#[test]
fn helper() {
    let Some(arg) = std::env::args().find(|arg| arg.starts_with("helper:")) else {
        return;
    };
    let parts: Vec<&str> = arg["helper:".len()..].split(':').collect();
    run_mode(&parts);
    std::process::exit(0);
}

fn allowed_cpus() -> Vec<u16> {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let list = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .unwrap_or("0")
        .trim()
        .to_owned();
    list.split(',')
        .flat_map(|part| {
            let (from, to) = part.split_once('-').unwrap_or((part, part));
            let (from, to) = (from.parse::<u16>().unwrap_or(0), to.parse::<u16>().unwrap_or(0));
            from..=to
        })
        .collect()
}

fn pin(cpu: usize) {
    let mut set = rustix::thread::CpuSet::new();
    set.set(cpu);
    let _ = rustix::thread::sched_setaffinity(None, &set);
}

fn block_term() {
    use nix::sys::signal::{SigSet, SigmaskHow, Signal as NixSignal, sigprocmask};
    let mut set = SigSet::empty();
    set.add(NixSignal::SIGTERM);
    let _ = sigprocmask(SigmaskHow::SIG_BLOCK, Some(&set), None);
}

fn hour() {
    std::thread::sleep(secs(3600));
}

fn spin(length: Duration) {
    let end = Instant::now() + length;
    while Instant::now() < end {
        std::hint::black_box(0_u64.wrapping_add(1));
    }
}

fn run_mode(parts: &[&str]) {
    let mode = parts[0];
    let big = vec![b'x'; 1 << 20];
    let stdout = || std::io::stdout();
    match mode {
        "work" => {
            for step in 0..50 {
                spin(Duration::from_millis(100));
                println!("ok {step}");
                let _ = std::io::stdout().flush();
            }
        }
        "exit" => {}
        "error" => {
            std::thread::sleep(Duration::from_millis(300));
            println!("COMPUTE ERROR");
            let _ = std::io::stdout().flush();
            std::thread::sleep(secs(5));
        }
        "oversized-stdout" => {
            let _ = stdout().write_all(&big);
            let _ = stdout().flush();
            std::thread::sleep(secs(5));
        }
        "oversized-stderr" => {
            let _ = std::io::stderr().write_all(&big);
            std::thread::sleep(secs(5));
        }
        "oversized-watched" => {
            std::fs::write("results.txt", &big).unwrap();
            std::thread::sleep(secs(5));
        }
        "affinity-then-oversized" | "compute-then-oversized" => {
            let text = if mode.starts_with("affinity") { "AFFINITY:9\n" } else { "COMPUTE ERROR\n" };
            let _ = std::io::stderr().write_all(text.as_bytes());
            std::thread::sleep(Duration::from_millis(50));
            let _ = stdout().write_all(&big);
            let _ = stdout().flush();
            std::thread::sleep(secs(5));
        }
        "escape" => {
            let to: usize = parts.get(2).and_then(|c| c.parse().ok()).unwrap_or(0);
            std::thread::sleep(Duration::from_millis(100));
            pin(to);
            spin(secs(5));
        }
        "pipe-descendant" => {
            use std::os::unix::process::CommandExt as _;
            let exe = std::env::current_exe().unwrap();
            let child = std::process::Command::new(exe)
                .process_group(0)
                .args(["--exact", HELPER, "--nocapture", "--test-threads=1", "helper:hold:0"])
                .spawn()
                .unwrap();
            std::fs::write("descendant.pid.tmp", child.id().to_string()).unwrap();
            std::fs::rename("descendant.pid.tmp", "descendant.pid").unwrap();
            hour();
        }
        "hold" => hour(),
        "watched-flood" => {
            block_term();
            let mut bytes = b"ok line\n".repeat(4 * 1024 * 1024 / 8);
            bytes.extend_from_slice(b"COMPUTE ERROR");
            std::fs::write("results.txt", bytes).unwrap();
            println!("WATCHED READY");
            let _ = std::io::stdout().flush();
            hour();
        }
        "watched-error" => {
            std::fs::write("results.txt", "COMPUTE ERROR\n").unwrap();
            hour();
        }
        _ => hour(),
    }
}

/// A backend that runs the test binary as a helper in `mode`, on the first CPU it is given.
struct Helper {
    mode: String,
    watched: bool,
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

    fn plan(&self, _: &Workload, _: &Path, cpus: &[CpuId]) -> Result<Plan, Error> {
        let cpu = cpus.first().map_or(0, |cpu| cpu.0);
        let mode = if self.mode.starts_with("escape") {
            let from = cpu;
            let to = allowed_cpus().into_iter().find(|c| *c != from).unwrap_or(from);
            format!("{}:{from}:{to}", self.mode)
        } else {
            format!("{}:{cpu}", self.mode)
        };
        Ok(Plan {
            program: std::env::current_exe().unwrap(),
            args: [
                "--exact".to_owned(),
                HELPER.to_owned(),
                "--nocapture".to_owned(),
                "--test-threads=1".to_owned(),
                format!("helper:{mode}"),
            ]
            .map(OsString::from)
            .to_vec(),
            inputs: Vec::new(),
            watched: if self.watched {
                vec![FileName::from_static("results.txt")]
            } else {
                Vec::new()
            },
        })
    }

    fn classify(&self, line: &str) -> Line {
        if line.contains("COMPUTE ERROR") {
            Line::ComputationError
        } else if let Some(cpu) = line.strip_prefix("AFFINITY:") {
            Line::AffinityError(cpu.parse().ok().map(CpuId))
        } else if line.starts_with("ok") {
            Line::Progress(line.to_owned())
        } else {
            Line::Other
        }
    }
}

fn timing() -> Timing {
    Timing {
        sample: Duration::from_millis(400),
        stall_grace: secs(3600),
        ..Timing::default()
    }
}

fn runner(mode: &str, watched: bool, timing: Timing, dir: &Path) -> Runner {
    let cpus = allowed_cpus();
    let first = cpus[0];
    let second = cpus.get(1).copied().unwrap_or(first);
    let topology = vec![
        togi_machine::CoreInfo {
            core: core(0),
            ccd: togi_machine::CcdId(0),
            cpus: vec![CpuId(first)],
        },
        togi_machine::CoreInfo {
            core: core(1),
            ccd: togi_machine::CcdId(0),
            cpus: vec![CpuId(second)],
        },
    ];
    let options = Options {
        dir: dir.join("trials"),
        backends: Backends::new().with(Arc::new(Helper {
            mode: mode.to_owned(),
            watched,
        })),
        topology,
        identity: None,
        pm_table: None,
    };
    let settings = Settings {
        timing,
        roots: Roots::default(),
        unscoped: true,
        user_manager: false,
    };
    Runner::build(options, settings, || {
        Linux::new(Proc::new("/proc".into(), ticks_per_second()), false)
    })
    .unwrap()
}

fn spec(regime: Regime, cores: &[u8], duration: Duration) -> togi_machine::TrialSpec {
    let mut spec = super::harness::spec(regime, cores, duration);
    let cpus = allowed_cpus();
    spec.cpus = match regime {
        Regime::R6 | Regime::R7 => cores
            .iter()
            .map(|&n| CpuId(if n == 0 { cpus[0] } else { cpus.get(1).copied().unwrap_or(cpus[0]) }))
            .collect(),
        _ => vec![CpuId(cpus[0])],
    };
    spec
}

struct Quiet;

impl togi_machine::Reporter for Quiet {
    fn progress(&mut self, _: &str) {}
    fn sample(&mut self, _: togi_machine::Sample) {}
    fn signal(&mut self, _: togi_machine::CoreId, _: Signal, _: &str) {}
}

fn run(
    runner: &mut Runner,
    spec: togi_machine::TrialSpec,
) -> (togi_machine::TrialResult, Result<Stopped, togi_machine::Error>, Duration) {
    let started = Instant::now();
    let mut trial = runner
        .start(&CancellationToken::new(), spec, nothing_runs())
        .map_err(|partial| partial.error)
        .unwrap();
    let result = match trial.wait(&CancellationToken::new(), &mut Quiet) {
        Ok(result) => result,
        Err(partial) => partial.value,
    };
    let stopped = trial.stop();
    (result, stopped, started.elapsed())
}

#[test]
fn a_working_helper_passes_a_failing_one_fails_and_an_exiting_one_exits_early() {
    let dir = tempfile::tempdir().unwrap();
    let mut work = runner("work", false, timing(), dir.path());
    let (result, stopped, _) = run(&mut work, spec(Regime::R1, &[0], Duration::from_millis(1500)));
    assert_eq!((result.failure, result.inconclusive.as_deref()), (None, None));
    assert!(result.escaped.is_empty(), "{:?} of {:?}", result.escaped, allowed_cpus());
    assert!(stopped.is_ok());

    let mut error = runner("error", false, timing(), dir.path());
    let (result, stopped, _) = run(&mut error, spec(Regime::R1, &[0], secs(10)));
    assert_eq!(result.failure.map(|f| f.signal), Some(Signal::ComputationError));
    assert!(stopped.is_ok());

    let mut exit = runner("exit", false, timing(), dir.path());
    let (result, stopped, _) = run(&mut exit, spec(Regime::R1, &[0], secs(10)));
    assert_eq!(result.failure.map(|f| f.signal), Some(Signal::UnexpectedExit));
    assert!(stopped.is_ok());
}

#[test]
fn a_helper_that_pins_itself_elsewhere_is_an_escape() {
    if allowed_cpus().len() < 2 {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut escaping = runner("escape", false, timing(), dir.path());
    let (result, stopped, _) = run(&mut escaping, spec(Regime::R1, &[0], secs(10)));
    assert_eq!(result.escaped.len(), 1);
    assert!(!allowed_cpus().is_empty());
    assert_eq!(result.failure, None);
    assert!(stopped.is_ok());
}

#[test]
fn oversized_output_on_any_source_is_inconclusive_with_the_exact_prefix_and_no_containment() {
    for (mode, log) in [
        ("oversized-stdout", Some("stdout.log")),
        ("oversized-stderr", Some("stderr.log")),
        ("oversized-watched", None),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut oversized = runner(mode, true, timing(), dir.path());
        let (result, stopped, elapsed) = run(&mut oversized, spec(Regime::R1, &[0], secs(30)));
        assert!(result.inconclusive.is_some(), "{mode}");
        assert_eq!((result.failure, result.escaped.len()), (None, 0), "{mode}");
        assert!(stopped.is_ok(), "{mode}");
        assert!(elapsed < secs(15), "{mode}: {elapsed:?}");
        if let Some(log) = log {
            let bytes = std::fs::read(dir.path().join("trials/t1/work").join(log)).unwrap();
            assert!((64 * 1024..64 * 1024 + 64).contains(&bytes.len()), "{mode}: {}", bytes.len());
            assert!(bytes.ends_with(&vec![b'x'; 1024]), "{mode}");
        }
    }
}

#[test]
fn affinity_and_computation_errors_beat_an_overlong_line() {
    for mode in ["affinity-then-oversized", "compute-then-oversized"] {
        let dir = tempfile::tempdir().unwrap();
        let mut helper = runner(mode, false, timing(), dir.path());
        let (result, stopped, _) = run(&mut helper, spec(Regime::R1, &[0], secs(30)));
        assert_eq!(result.inconclusive, None, "{mode}");
        if mode.starts_with("affinity") {
            assert_eq!(result.escaped, [CpuId(9)]);
        } else {
            assert_eq!(result.failure.map(|f| f.signal), Some(Signal::ComputationError));
        }
        assert!(stopped.is_ok());
    }
}

#[test]
fn r6_workers_are_stopped_before_the_trial_starts_to_count() {
    let dir = tempfile::tempdir().unwrap();
    let mut idle = runner("work", false, timing(), dir.path());
    let mut trial = idle
        .start(
            &CancellationToken::new(),
            spec(Regime::R6, &[0, 1], Duration::from_secs(2)),
            nothing_runs(),
        )
        .map_err(|partial| partial.error)
        .unwrap();
    for instance in trial.started().instances {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", instance.pid)).unwrap();
        let state = stat.rsplit(')').next().unwrap().trim().chars().next().unwrap();
        assert_eq!(state, 'T', "worker {} is stopped before wait", instance.pid);
    }
    let result = trial.wait(&CancellationToken::new(), &mut Quiet).unwrap();
    assert_eq!(result.stops, 2 + 1, "two initial stops and the one burst stop");
    assert_eq!(result.conts, 1);
    assert!(trial.stop().is_ok());
}

fn kill_pid(pid: u32) {
    if let Some(pid) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
}

#[test]
fn a_detached_pipe_holder_makes_the_drain_unconfirmed_and_the_stop_containment() {
    let dir = tempfile::tempdir().unwrap();
    let short = Timing {
        teardown: secs(2),
        drain: secs(1),
        stop_grace: Duration::from_millis(300),
        ..timing()
    };
    let mut holder = runner("pipe-descendant", false, short, dir.path());
    let pid_file: PathBuf = dir.path().join("trials/t1/work/descendant.pid");
    let mut trial = holder
        .start(
            &CancellationToken::new(),
            spec(Regime::R1, &[0], Duration::from_millis(1500)),
            nothing_runs(),
        )
        .map_err(|partial| partial.error)
        .unwrap();
    let begun = Instant::now();
    let _ = trial.wait(&CancellationToken::new(), &mut Quiet);
    let stopped = trial.stop();
    let descendant: u32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    kill_pid(descendant);
    assert!(
        matches!(stopped, Err(togi_machine::Error::Containment)),
        "the group kill cannot reach it and the pipes stay open"
    );
    assert!(begun.elapsed() < secs(10));
}

#[test]
fn a_flooding_watched_file_is_drained_once_while_the_helper_ignores_term() {
    let dir = tempfile::tempdir().unwrap();
    let mut flooding = runner("watched-flood", true, timing(), dir.path());
    let (result, stopped, elapsed) = run(&mut flooding, spec(Regime::R1, &[0], Duration::from_secs(2)));
    assert_eq!(result.failure.map(|f| f.signal), Some(Signal::ComputationError));
    assert!(stopped.is_ok(), "{stopped:?}");
    assert!(elapsed < secs(15), "{elapsed:?}");
}

#[test]
fn a_relative_trial_directory_works_and_the_watched_file_lands_in_the_work_directory() {
    let relative = PathBuf::from(format!("trial-process-{}", std::process::id()));
    let cwd = std::env::current_dir().unwrap();
    let mut watching = runner("watched-error", true, timing(), &relative);
    let (result, stopped, _) = run(&mut watching, spec(Regime::R1, &[0], secs(10)));
    assert_eq!(result.failure.map(|f| f.signal), Some(Signal::ComputationError));
    assert!(stopped.is_ok());
    assert!(cwd.join(&relative).join("trials/t1/work/results.txt").exists());
    std::fs::remove_dir_all(cwd.join(&relative)).unwrap();
}

#[test]
fn a_zombie_leader_keeps_its_group_signalable_until_it_is_reaped() {
    use crate::launch::LaunchCommand;
    use crate::os::{Child as _, GroupSignal, Os as _};
    let os = Linux::new(Proc::new("/proc".into(), ticks_per_second()), false);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let command = LaunchCommand {
            argv: vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
            cwd: "/".into(),
            pin: Vec::new(),
        };
        let child = os.launch(&command).unwrap();
        child.exited().await.unwrap();
        assert!(child.exit().unwrap().is_some());
        assert!(matches!(
            child.signal_group(GroupSignal::Cont),
            Ok(()) | Err(crate::os::SignalError::Gone)
        ));
        assert!(child.reap().is_ok());
    });
}

#[test]
fn systemctl_calls_are_bounded_and_a_timed_out_missing_scope_text_is_not_success() {
    use crate::id::ScopeName;
    use crate::os::{KillSignal, Os as _, Systemd};
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("systemctl");
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'Unit togi-trial-t1.scope not loaded.'\nexec sleep 60\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let os = Linux::new(Proc::new("/proc".into(), ticks_per_second()), false).with_systemctl(&script);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let begun = Instant::now();
    let outcome = runtime.block_on(os.systemd(
        &Systemd::Kill {
            scope: ScopeName::from_unit("togi-trial-t1.scope").unwrap(),
            signal: KillSignal::Kill,
        },
        tokio::time::Instant::now() + secs(1),
    ));
    assert!(matches!(outcome, Err(togi_command::Error::TimedOut { .. })));
    assert!(begun.elapsed() >= secs(1) && begun.elapsed() < secs(3));
    let _ = (crate_error, Roots::default());
}
