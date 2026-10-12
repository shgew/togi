//! Real processes through the private unscoped path: the test binary re-executes itself as a
//! helper, so no host scope, root or systemd is involved. None of this is containment.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use togi_backend::{Backend, Error, FileName, Line, Plan, Ready};
use togi_machine::{
    CancellationToken, CpuId, Partial, Regime, Signal, Stopped, TrialResult, Trials, Workload,
};

use super::harness::{core, crate_error, nothing_runs, own_identity};
use crate::launch::LaunchCommand;
use crate::native::Settings;
use crate::os::linux::{Leader, Linux};
use crate::os::{Child as _, ExitInfo, GroupSignal, LaunchFailure, Os, Systemd};
use crate::procfs::{Proc, ProcError, ScopeProcess, Threads, Usage, parse_stat, ticks_per_second};
use crate::runner::{Backends, Options, Runner};
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

fn helper_args(mode: &str) -> Vec<OsString> {
    [
        "--exact",
        HELPER,
        "--nocapture",
        "--test-threads=1",
        &format!("helper:{mode}"),
    ]
    .map(OsString::from)
    .to_vec()
}

/// The command that runs the test binary as a helper in `mode`, working in `cwd`.
pub(super) fn helper_command(mode: &str, cwd: &Path) -> LaunchCommand {
    let mut argv =
        vec![std::env::current_exe().map_or_else(|_| "/proc/self/exe".into(), Into::into)];
    argv.extend(helper_args(mode));
    LaunchCommand {
        argv,
        cwd: cwd.to_owned(),
        pin: Vec::new(),
    }
}

pub(super) fn allowed_cpus() -> Vec<u16> {
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
            let (from, to) = (
                from.parse::<u16>().unwrap_or(0),
                to.parse::<u16>().unwrap_or(0),
            );
            from..=to
        })
        .collect()
}

fn pin(cpu: usize) {
    let mut set = rustix::thread::CpuSet::new();
    set.set(cpu);
    let _ = rustix::thread::sched_setaffinity(None, &set);
}

static TERMED: AtomicBool = AtomicBool::new(false);

extern "C" fn note_term(_: i32) {
    TERMED.store(true, Ordering::SeqCst);
}

#[expect(
    unsafe_code,
    reason = "installing a signal disposition is an unsafe libc call"
)]
fn set_term(handler: nix::sys::signal::SigHandler) {
    use nix::sys::signal::{SaFlags, SigAction, SigSet, Signal as NixSignal, sigaction};
    let action = SigAction::new(handler, SaFlags::empty(), SigSet::empty());
    // SAFETY: the handler is SIG_IGN or `note_term`, which only stores to an atomic; both are
    // async-signal-safe, and the disposition is process-wide, not a thread's mask.
    let _ = unsafe { sigaction(NixSignal::SIGTERM, &action) };
}

fn ignore_term() {
    set_term(nix::sys::signal::SigHandler::SigIgn);
}

fn catch_term() {
    set_term(nix::sys::signal::SigHandler::Handler(note_term));
}

/// Blocks until the teardown's SIGTERM arrived; the helper gives up after 20 s.
fn wait_term() {
    let end = Instant::now() + secs(20);
    while !TERMED.load(Ordering::SeqCst) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
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

/// A detached helper that is killed when dropped, until `release` hands it over.
struct Holder(Option<std::process::Child>);

impl Holder {
    fn id(&self) -> Option<u32> {
        self.0.as_ref().map(std::process::Child::id)
    }

    fn release(mut self) {
        self.0.take();
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_holder(quiet: bool) -> std::io::Result<Holder> {
    use std::os::unix::process::CommandExt as _;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.process_group(0).args(helper_args("hold:0"));
    if quiet {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the helper starts a detached descendant that outlives it on purpose"
    )]
    let child = command.spawn()?;
    Ok(Holder(Some(child)))
}

fn publish_pid(pid: u32) -> std::io::Result<()> {
    std::fs::write("descendant.pid.tmp", pid.to_string())?;
    std::fs::rename("descendant.pid.tmp", "descendant.pid")
}

/// Starts a detached descendant and publishes its pid; the descendant is killed again if that
/// fails, so a failed setup leaves nothing behind.
fn hold_a_descendant(quiet: bool) {
    let Ok(holder) = spawn_holder(quiet) else {
        std::process::exit(2);
    };
    if matches!(holder.id().map(publish_pid), Some(Ok(()))) {
        holder.release();
    } else {
        drop(holder);
        std::process::exit(2);
    }
    hour();
}

fn emit_oversized() {
    let big = vec![b'x'; 1 << 20];
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(&big);
    let _ = stdout.flush();
}

fn emit_classified(text: &str) {
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(text.as_bytes());
    let _ = stderr.flush();
}

fn run_mode(parts: &[&str]) {
    let mode = parts[0];
    let big = vec![b'x'; 1 << 20];
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
            emit_oversized();
            std::thread::sleep(secs(5));
        }
        "oversized-stderr" => {
            let _ = std::io::stderr().write_all(&big);
            std::thread::sleep(secs(5));
        }
        "oversized-watched" => {
            let _ = std::fs::write("results.txt", &big);
            std::thread::sleep(secs(5));
        }
        "affinity-then-oversized"
        | "compute-then-oversized"
        | "oversized-then-affinity"
        | "oversized-then-compute" => {
            catch_term();
            let text = if mode.contains("affinity") {
                "AFFINITY:9\n"
            } else {
                "COMPUTE ERROR\n"
            };
            if mode.starts_with("oversized") {
                emit_oversized();
                wait_term();
                emit_classified(text);
            } else {
                emit_classified(text);
                wait_term();
                emit_oversized();
            }
        }
        "escape" => {
            let to: usize = parts.get(2).and_then(|c| c.parse().ok()).unwrap_or(0);
            std::thread::sleep(Duration::from_millis(100));
            pin(to);
            spin(secs(5));
        }
        "pipe-descendant" => hold_a_descendant(false),
        "scope-owner" => hold_a_descendant(true),
        "hold" => hour(),
        "watched-flood" => {
            ignore_term();
            let mut bytes = b"ok line\n".repeat(4 * 1024 * 1024 / 8);
            bytes.extend_from_slice(b"COMPUTE ERROR");
            if std::fs::write("results.txt", bytes).is_err() {
                std::process::exit(2);
            }
            println!("WATCHED READY");
            let _ = std::io::stdout().flush();
            hour();
        }
        "watched-error" => {
            let _ = std::fs::write("results.txt", "COMPUTE ERROR\n");
            hour();
        }
        _ => hour(),
    }
}

/// A process it was handed, killed when dropped if it is still the same process.
pub(super) struct Escaped {
    pid: u32,
    starttime: u64,
}

fn stat_of(pid: u32) -> Option<crate::procfs::Stat> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&text).ok()
}

impl Escaped {
    /// `None` when the process is already gone.
    pub(super) fn adopt(pid: u32) -> Option<Self> {
        let stat = stat_of(pid)?;
        Some(Self {
            pid,
            starttime: stat.starttime,
        })
    }

    pub(super) fn pid(&self) -> u32 {
        self.pid
    }

    pub(super) fn group(&self) -> Option<i64> {
        stat_of(self.pid).map(|stat| stat.pgrp)
    }

    pub(super) fn alive(&self) -> bool {
        stat_of(self.pid).is_some_and(|stat| {
            stat.starttime == self.starttime && !matches!(stat.state, 'Z' | 'X')
        })
    }

    pub(super) fn kill(&self) {
        if self.alive()
            && let Some(pid) = i32::try_from(self.pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

impl Drop for Escaped {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Whether the process has SIGTERM in its ignored set, which `/proc` reports for the process as a
/// whole, not for one thread.
fn ignores_term(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("SigIgn:"))
                .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        })
        .is_some_and(|mask| mask & (1 << (nix::sys::signal::Signal::SIGTERM as i32 - 1)) != 0)
}

pub(super) fn wait_until(limit: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

/// What a backend does with the first line of a watched flood: the token it cancels, and whether
/// it did.
struct CancelOnFirstLine {
    token: CancellationToken,
    fired: Arc<AtomicBool>,
}

/// A backend that runs the test binary as a helper in `mode`, on the first CPU it is given.
pub(super) struct Helper {
    mode: String,
    watched: bool,
    cancelling: Option<CancelOnFirstLine>,
}

impl Helper {
    pub(super) fn new(mode: &str, watched: bool) -> Self {
        Self {
            mode: mode.to_owned(),
            watched,
            cancelling: None,
        }
    }

    fn cancelling(mut self, token: &CancellationToken) -> (Self, Arc<AtomicBool>) {
        let fired = Arc::new(AtomicBool::new(false));
        self.cancelling = Some(CancelOnFirstLine {
            token: token.clone(),
            fired: Arc::clone(&fired),
        });
        (self, fired)
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

    fn plan(&self, _: &Workload, _: &Path, cpus: &[togi_machine::CpuId]) -> Result<Plan, Error> {
        let cpu = cpus.first().map_or(0, |cpu| cpu.0);
        let mode = if self.mode.starts_with("escape") {
            let from = cpu;
            let to = allowed_cpus()
                .into_iter()
                .find(|c| *c != from)
                .unwrap_or(from);
            format!("{}:{from}:{to}", self.mode)
        } else {
            format!("{}:{cpu}", self.mode)
        };
        Ok(Plan {
            program: std::env::current_exe().unwrap(),
            args: helper_args(&mode),
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
            if let Some(cancelling) = &self.cancelling
                && !cancelling.fired.swap(true, Ordering::SeqCst)
            {
                cancelling.token.cancel();
            }
            Line::Progress(line.to_owned())
        } else {
            Line::Other
        }
    }
}

pub(super) fn timing() -> Timing {
    Timing {
        sample: Duration::from_millis(400),
        stall_grace: secs(3600),
        ..Timing::default()
    }
}

fn topology() -> Vec<togi_machine::CoreInfo> {
    let cpus = allowed_cpus();
    let first = cpus[0];
    let second = cpus.get(1).copied().unwrap_or(first);
    vec![
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
    ]
}

/// A runner over `make_os`; `scoped` has the trials start through scope commands, which a seam
/// `Os` then runs as plain programs, and gives the runner a backend account.
pub(super) fn runner_over<O: Os>(
    helper: Helper,
    scoped: bool,
    timing: Timing,
    dir: &Path,
    make_os: impl FnOnce() -> O + Send + 'static,
) -> Runner {
    let options = Options {
        dir: dir.join("trials"),
        backends: Backends::new().with(Arc::new(helper)),
        topology: topology(),
        identity: scoped.then(own_identity),
        pm_table: None,
    };
    let settings = Settings {
        timing,
        roots: Roots::default(),
        unscoped: !scoped,
        user_manager: false,
    };
    Runner::build(options, settings, make_os).unwrap()
}

pub(super) fn linux() -> Linux {
    Linux::new(Proc::new("/proc".into(), ticks_per_second()), false)
}

fn runner(mode: &str, watched: bool, timing: Timing, dir: &Path) -> Runner {
    runner_over(Helper::new(mode, watched), false, timing, dir, linux)
}

fn spec(regime: Regime, cores: &[u8], duration: Duration) -> togi_machine::TrialSpec {
    let mut spec = super::harness::spec(regime, cores, duration);
    let cpus = allowed_cpus();
    spec.cpus = match regime {
        Regime::R6 | Regime::R7 => cores
            .iter()
            .map(|&n| {
                CpuId(if n == 0 {
                    cpus[0]
                } else {
                    cpus.get(1).copied().unwrap_or(cpus[0])
                })
            })
            .collect(),
        _ => vec![CpuId(cpus[0])],
    };
    spec
}

/// What a trial reported while it was waited for.
#[derive(Default)]
struct Record {
    signals: Vec<(Signal, String)>,
}

impl togi_machine::Reporter for Record {
    fn progress(&mut self, _: &str) {}
    fn sample(&mut self, _: togi_machine::Sample) {}
    fn signal(&mut self, _: togi_machine::CoreId, signal: Signal, detail: &str) {
        self.signals.push((signal, detail.to_owned()));
    }
}

/// A trial run to its end: what `wait` returned, error included, and what `stop` proved.
struct Outcome {
    result: TrialResult,
    waited: Option<togi_machine::Error>,
    stopped: Result<Stopped, togi_machine::Error>,
    elapsed: Duration,
    signals: Vec<(Signal, String)>,
}

impl Outcome {
    fn failure(&self) -> Option<Signal> {
        self.result.failure.map(|failure| failure.signal)
    }

    fn assert_clean(&self, what: &str) {
        assert!(
            self.waited.is_none(),
            "{what}: wait failed: {:?}",
            self.waited
        );
        assert!(
            self.stopped.is_ok(),
            "{what}: stop failed: {:?}",
            self.stopped
        );
    }
}

fn run(runner: &mut Runner, spec: togi_machine::TrialSpec) -> Outcome {
    let started = Instant::now();
    let mut trial = runner
        .start(&CancellationToken::new(), spec, nothing_runs())
        .map_err(|partial| partial.error)
        .unwrap();
    let mut record = Record::default();
    let (result, waited) = match trial.wait(&CancellationToken::new(), &mut record) {
        Ok(result) => (result, None),
        Err(Partial { value, error }) => (value, Some(error)),
    };
    let stopped = trial.stop();
    Outcome {
        result,
        waited,
        stopped,
        elapsed: started.elapsed(),
        signals: record.signals,
    }
}

/// The crate errors a failed wait carries, in order.
fn crate_errors(error: &togi_machine::Error) -> Vec<&crate::Error> {
    let togi_machine::Error::Other(inner) = error else {
        return Vec::new();
    };
    match inner
        .source()
        .and_then(|source| source.downcast_ref::<crate::Error>())
    {
        Some(crate::Error::Joined(joined)) => joined.errors().iter().collect(),
        Some(other) => vec![other],
        None => crate_error(error).into_iter().collect(),
    }
}

/// The oversized-line diagnostics a failed wait carries.
fn cap_diagnostics(error: &togi_machine::Error) -> Vec<&str> {
    crate_errors(error)
        .into_iter()
        .filter_map(|error| match error {
            crate::Error::Output(output) => Some(output.diagnostics()),
            _ => None,
        })
        .flatten()
        .map(String::as_str)
        .filter(|text| text.contains("exceeds 64 KiB"))
        .collect()
}

fn assert_capped_prefix(dir: &Path, log: &str, what: &str) {
    let bytes = std::fs::read(dir.join("trials/t1/work").join(log)).unwrap();
    assert!(
        (64 * 1024..64 * 1024 + 64).contains(&bytes.len()),
        "{what}: {}",
        bytes.len()
    );
    assert!(bytes.ends_with(&[b'x'; 1024]), "{what}");
}

#[test]
fn real_process_a_working_helper_passes_a_failing_one_fails_and_an_exiting_one_exits_early() {
    let dir = crate::testing::tempdir();
    let mut work = runner("work", false, timing(), dir.path());
    let outcome = run(
        &mut work,
        spec(Regime::R1, &[0], Duration::from_millis(1500)),
    );
    outcome.assert_clean("work");
    assert_eq!(
        (
            outcome.result.failure,
            outcome.result.inconclusive.as_deref()
        ),
        (None, None)
    );
    assert!(
        outcome.result.escaped.is_empty(),
        "{:?} of {:?}",
        outcome.result.escaped,
        allowed_cpus()
    );

    let mut error = runner("error", false, timing(), dir.path());
    let outcome = run(&mut error, spec(Regime::R1, &[0], secs(10)));
    outcome.assert_clean("error");
    assert_eq!(outcome.failure(), Some(Signal::ComputationError));

    let mut exit = runner("exit", false, timing(), dir.path());
    let outcome = run(&mut exit, spec(Regime::R1, &[0], secs(10)));
    outcome.assert_clean("exit");
    assert_eq!(outcome.failure(), Some(Signal::UnexpectedExit));
}

#[test]
fn real_process_a_helper_that_pins_itself_elsewhere_is_an_escape() {
    if allowed_cpus().len() < 2 {
        return;
    }
    let dir = crate::testing::tempdir();
    let mut escaping = runner("escape", false, timing(), dir.path());
    let outcome = run(&mut escaping, spec(Regime::R1, &[0], secs(10)));
    outcome.assert_clean("escape");
    assert_eq!(outcome.result.escaped, [CpuId(allowed_cpus()[1])]);
    assert_eq!(outcome.result.failure, None);
}

#[test]
fn real_process_oversized_output_on_any_source_is_inconclusive_with_the_exact_prefix_and_no_containment()
 {
    for (mode, log) in [
        ("oversized-stdout", Some("stdout.log")),
        ("oversized-stderr", Some("stderr.log")),
        ("oversized-watched", None),
    ] {
        let dir = crate::testing::tempdir();
        let mut oversized = runner(mode, true, timing(), dir.path());
        let outcome = run(&mut oversized, spec(Regime::R1, &[0], secs(30)));
        assert!(outcome.result.inconclusive.is_some(), "{mode}");
        assert_eq!(
            (outcome.result.failure, outcome.result.escaped.len()),
            (None, 0),
            "{mode}"
        );
        assert!(outcome.stopped.is_ok(), "{mode}");
        let Some(waited) = &outcome.waited else {
            panic!("{mode}: the cap must surface as an error of the wait");
        };
        assert_eq!(cap_diagnostics(waited).len(), 1, "{mode}: {waited:?}");
        assert!(outcome.elapsed < secs(15), "{mode}: {:?}", outcome.elapsed);
        if let Some(log) = log {
            assert_capped_prefix(dir.path(), log, mode);
        }
    }
}

/// Go `TestProcessOutputLimitEvidencePrecedence`: the helper makes the cap and the classified
/// failure both reach the selector, in either order, because it writes the second only after the
/// teardown's SIGTERM, which only the first can have caused.
#[test]
fn real_process_affinity_and_computation_errors_beat_an_overlong_line() {
    for mode in [
        "affinity-then-oversized",
        "compute-then-oversized",
        "oversized-then-affinity",
        "oversized-then-compute",
    ] {
        let dir = crate::testing::tempdir();
        let mut helper = runner(mode, false, timing(), dir.path());
        let outcome = run(&mut helper, spec(Regime::R1, &[0], secs(30)));
        outcome.assert_clean(mode);
        assert_eq!(outcome.result.inconclusive, None, "{mode}");
        if mode.contains("affinity") {
            assert_eq!(outcome.result.escaped, [CpuId(9)], "{mode}");
            assert_eq!(outcome.failure(), None, "{mode}");
        } else {
            assert_eq!(outcome.failure(), Some(Signal::ComputationError), "{mode}");
            assert!(outcome.result.escaped.is_empty(), "{mode}");
            assert_eq!(
                outcome
                    .signals
                    .iter()
                    .filter(|(signal, _)| *signal == Signal::ComputationError)
                    .count(),
                1,
                "{mode}"
            );
        }
        assert_capped_prefix(dir.path(), "stdout.log", mode);
        assert!(outcome.elapsed < secs(15), "{mode}: {:?}", outcome.elapsed);
    }
}

#[test]
fn real_process_r6_workers_are_stopped_before_the_trial_starts_to_count() {
    let dir = crate::testing::tempdir();
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
        let stopped = wait_until(secs(1), || {
            std::fs::read_to_string(format!("/proc/{}/stat", instance.pid))
                .ok()
                .and_then(|stat| stat.rsplit(')').next().map(str::trim).map(str::to_owned))
                .is_some_and(|rest| rest.starts_with('T'))
        });
        assert!(stopped, "worker {} is stopped before wait", instance.pid);
    }
    let result = trial
        .wait(&CancellationToken::new(), &mut Record::default())
        .unwrap();
    assert_eq!(
        result.stops,
        2 + 1,
        "two initial stops and the one burst stop"
    );
    assert_eq!(result.conts, 1);
    assert!(trial.stop().is_ok());
}

fn published_holder(file: &Path) -> Escaped {
    assert!(
        wait_until(secs(10), || file.exists()),
        "the helper published no detached descendant"
    );
    let pid: u32 = std::fs::read_to_string(file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    Escaped::adopt(pid).expect("the detached descendant is alive")
}

#[test]
fn real_process_a_detached_pipe_holder_makes_the_drain_unconfirmed_and_the_stop_containment() {
    let dir = crate::testing::tempdir();
    let short = Timing {
        teardown: secs(2),
        drain: secs(1),
        stop_grace: Duration::from_millis(300),
        ..timing()
    };
    let mut holder = runner("pipe-descendant", false, short, dir.path());
    let mut trial = holder
        .start(
            &CancellationToken::new(),
            spec(Regime::R1, &[0], Duration::from_millis(1500)),
            nothing_runs(),
        )
        .map_err(|partial| partial.error)
        .unwrap();
    let begun = Instant::now();
    let descendant = published_holder(&dir.path().join("trials/t1/work/descendant.pid"));
    let waited = trial.wait(&CancellationToken::new(), &mut Record::default());
    let stopped = trial.stop();
    let alive_after_the_stop = descendant.alive();
    descendant.kill();
    assert!(
        matches!(
            waited,
            Err(Partial {
                error: togi_machine::Error::Containment(_),
                ..
            })
        ),
        "the wait reports the unconfirmed cleanup: {waited:?}"
    );
    assert!(
        matches!(stopped, Err(togi_machine::Error::Containment(_))),
        "the group kill cannot reach it and the pipes stay open"
    );
    assert!(
        alive_after_the_stop,
        "the detached descendant outlives the stop"
    );
    assert!(begun.elapsed() < secs(10));
}

#[test]
fn real_process_the_watched_flood_helper_survives_a_group_term_and_only_sigkill_ends_it() {
    let dir = crate::testing::tempdir();
    let os = linux();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _enter = runtime.enter();
    let leader = os
        .launch(&helper_command("watched-flood", dir.path()))
        .unwrap();
    let pid = leader.pid();
    assert!(
        wait_until(secs(10), || ignores_term(pid)),
        "the helper ignores SIGTERM for the whole process"
    );
    assert!(leader.signal_group(GroupSignal::Term).is_ok());
    assert!(leader.signal_group(GroupSignal::Kill).is_ok());
    assert!(wait_until(secs(10), || leader
        .exit()
        .is_ok_and(|exit| exit.is_some())));
    assert_eq!(
        leader.reap().unwrap(),
        ExitInfo::Signal(9),
        "SIGTERM did not end it, SIGKILL did"
    );
}

#[test]
fn real_process_a_flooding_watched_file_is_drained_once_while_the_helper_ignores_term() {
    let dir = crate::testing::tempdir();
    let mut flooding = runner("watched-flood", true, timing(), dir.path());
    let outcome = run(
        &mut flooding,
        spec(Regime::R1, &[0], Duration::from_secs(2)),
    );
    outcome.assert_clean("flood");
    assert_eq!(outcome.failure(), Some(Signal::ComputationError));
    assert_eq!(
        outcome.signals,
        [(Signal::ComputationError, "COMPUTE ERROR".to_owned())]
    );
    assert!(outcome.elapsed < secs(15), "{:?}", outcome.elapsed);
    assert!(
        outcome.elapsed >= secs(2) + timing().stop_grace,
        "the helper ignored SIGTERM, so the whole grace passed: {:?}",
        outcome.elapsed
    );
}

/// Go `TestProcessWatchedFloodCancellation`: the backend's classifier cancels the trial while the
/// first batch of the flood is being classified.
#[test]
fn real_process_cancelling_during_a_watched_flood_ends_supervision_and_the_teardown_drains_the_backlog()
 {
    let dir = crate::testing::tempdir();
    let cancel = CancellationToken::new();
    let (helper, fired) = Helper::new("watched-flood", true).cancelling(&cancel);
    let flood_timing = Timing {
        stop_grace: Duration::from_millis(300),
        ..timing()
    };
    let mut flooding = runner_over(helper, false, flood_timing, dir.path(), linux);
    let mut trial = flooding
        .start(&cancel, spec(Regime::R1, &[0], secs(30)), nothing_runs())
        .map_err(|partial| partial.error)
        .unwrap();
    let begun = Instant::now();
    let mut record = Record::default();
    let waited = trial.wait(&cancel, &mut record);
    let elapsed = begun.elapsed();
    let Err(Partial {
        value: result,
        error,
    }) = waited
    else {
        panic!("a cancelled trial reports its cancellation");
    };
    assert!(
        fired.load(Ordering::SeqCst),
        "the flood's first line cancelled it"
    );
    assert!(
        matches!(error, togi_machine::Error::Cancelled),
        "only the cancellation, no containment: {error:?}"
    );
    assert_eq!(
        result.failure.map(|failure| failure.signal),
        Some(Signal::ComputationError),
        "the unterminated last line of the drained backlog"
    );
    assert_eq!(result.inconclusive, None);
    assert!(
        result.ran < timing().sample + secs(1),
        "supervision ended promptly: {:?}",
        result.ran
    );
    assert!(elapsed < secs(15), "{elapsed:?}");
    assert!(
        elapsed >= Duration::from_millis(300),
        "the helper ignored SIGTERM, so the whole grace passed: {elapsed:?}"
    );
    assert_eq!(
        record.signals,
        [(Signal::ComputationError, "COMPUTE ERROR".to_owned())],
        "reported exactly once"
    );
    assert!(trial.stop().is_ok());
}

fn relative_to_cwd(target: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let mut relative: PathBuf = cwd
        .components()
        .filter(|part| matches!(part, Component::Normal(_)))
        .map(|_| "..")
        .collect();
    relative.push(target.canonicalize().unwrap().strip_prefix("/").unwrap());
    relative
}

#[test]
fn real_process_a_relative_trial_directory_works_and_the_watched_file_lands_in_the_work_directory()
{
    let dir = crate::testing::tempdir();
    let relative = relative_to_cwd(dir.path());
    assert!(relative.is_relative());
    let mut watching = runner("watched-error", true, timing(), &relative);
    let outcome = run(&mut watching, spec(Regime::R1, &[0], secs(10)));
    outcome.assert_clean("watched-error");
    assert_eq!(outcome.failure(), Some(Signal::ComputationError));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("trials/t1/work/results.txt")).unwrap(),
        "COMPUTE ERROR\n"
    );
}

/// The ownership replacement of Go `TestProcessOwnedGroupIdentity`: the unreaped leader keeps the
/// group, so a same-group descendant is still reached.
#[test]
fn real_process_a_zombie_leader_keeps_its_group_signalable_until_it_is_reaped() {
    let dir = crate::testing::tempdir();
    let os = linux();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let command = LaunchCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "sleep 3600 & echo $! > descendant.pid; exit 0".into(),
            ],
            cwd: dir.path().to_owned(),
            pin: Vec::new(),
        };
        let child = os.launch(&command).unwrap();
        child.exited().await.unwrap();
        assert!(child.exit().unwrap().is_some(), "the leader is a zombie");
        let descendant = published_holder(&dir.path().join("descendant.pid"));
        assert_eq!(
            descendant.group(),
            Some(i64::from(child.pid())),
            "the descendant is in the leader's group"
        );
        assert!(descendant.alive());
        assert!(
            child.signal_group(GroupSignal::Term).is_ok(),
            "an unreaped leader's group is still signaled"
        );
        assert!(
            wait_until(secs(10), || !descendant.alive()),
            "the descendant {} ended by that signal",
            descendant.pid()
        );
        assert_eq!(child.reap().unwrap(), ExitInfo::Code(0));
    });
}

const INJECTED: &str = "injected second-instance start failure";

#[derive(Default)]
struct RollbackState {
    launches: AtomicUsize,
    failed_at: Mutex<Option<Instant>>,
    systemd: Mutex<Vec<Systemd>>,
    holders: Mutex<Vec<Escaped>>,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Real launches of the program inside each scope command, the second one refused, and every
/// systemd call recorded and answered with success: scope signals reach nothing.
struct Rollback {
    linux: Linux,
    state: Arc<RollbackState>,
}

impl Rollback {
    fn first_launch(&self, command: &LaunchCommand) -> Result<Leader, LaunchFailure<Leader>> {
        let program = command.argv.iter().position(|arg| arg == "--").map_or_else(
            || command.argv.clone(),
            |at| command.argv[at + 1..].to_vec(),
        );
        let leader = self.linux.launch(&LaunchCommand {
            argv: program,
            cwd: command.cwd.clone(),
            pin: command.pin.clone(),
        })?;
        let published = command.cwd.join("descendant.pid");
        if !wait_until(secs(10), || published.exists()) {
            return Err(LaunchFailure {
                child: Some(leader),
                source: std::io::Error::other("the first helper published no detached holder"),
            });
        }
        let holder = std::fs::read_to_string(&published)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .and_then(Escaped::adopt);
        match holder {
            Some(holder) => {
                locked(&self.state.holders).push(holder);
                Ok(leader)
            }
            None => Err(LaunchFailure {
                child: Some(leader),
                source: std::io::Error::other("the detached holder is not running"),
            }),
        }
    }
}

impl Os for Rollback {
    type Child = Leader;

    fn launch(&self, command: &LaunchCommand) -> Result<Leader, LaunchFailure<Leader>> {
        if self.state.launches.fetch_add(1, Ordering::SeqCst) == 0 {
            self.first_launch(command)
        } else {
            *locked(&self.state.failed_at) = Some(Instant::now());
            Err(std::io::Error::other(INJECTED).into())
        }
    }

    fn in_scope(&self, _: u32, _: &crate::id::ScopeName) -> Result<bool, ProcError> {
        Ok(true)
    }

    fn usage(&self, pid: u32) -> Result<Usage, ProcError> {
        self.linux.usage(pid)
    }

    fn threads(&self, pid: u32) -> Result<Threads, ProcError> {
        self.linux.threads(pid)
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the trait's operation is async; this seam answers at once"
    )]
    async fn systemd(
        &self,
        operation: &Systemd,
        _: tokio::time::Instant,
    ) -> Result<togi_command::Output, togi_command::Error> {
        use std::os::unix::process::ExitStatusExt as _;
        locked(&self.state.systemd).push(operation.clone());
        Ok(togi_command::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    async fn scope_processes(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<Vec<ScopeProcess>, ProcError> {
        self.linux.scope_processes(deadline).await
    }

    async fn survivors(
        &self,
        captured: &[ScopeProcess],
        deadline: tokio::time::Instant,
    ) -> Result<usize, ProcError> {
        self.linux.survivors(captured, deadline).await
    }
}

fn failures(error: &togi_machine::Error) -> Option<&[crate::error::CleanupFailure]> {
    match error {
        togi_machine::Error::Containment(source) => {
            if let Some(containment) = source.downcast_ref::<Arc<crate::Containment>>() {
                return Some(containment.failures());
            }
            match source.downcast_ref::<crate::Error>() {
                Some(crate::Error::Joined(joined)) => {
                    joined.errors().iter().find_map(|error| match error {
                        crate::Error::Containment(containment) => Some(containment.failures()),
                        _ => None,
                    })
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Go `TestProcessPartialStartRollback`: the first real helper has created a detached holder of
/// its pipes when the second launch fails.
#[test]
fn real_process_a_failed_second_launch_keeps_its_cause_first_targets_both_scopes_and_is_containment_while_a_holder_lives()
 {
    let dir = crate::testing::tempdir();
    let state = Arc::new(RollbackState::default());
    let rollback_timing = Timing {
        teardown: secs(2),
        drain: secs(1),
        stop_grace: Duration::from_millis(300),
        ..timing()
    };
    let teardown = rollback_timing.teardown;
    let make = {
        let state = Arc::clone(&state);
        move || Rollback {
            linux: linux(),
            state,
        }
    };
    let mut rolled_back = runner_over(
        Helper::new("pipe-descendant", false),
        true,
        rollback_timing,
        dir.path(),
        make,
    );
    let started = rolled_back.start(
        &CancellationToken::new(),
        spec(Regime::R7, &[0, 1], secs(60)),
        nothing_runs(),
    );
    let returned = Instant::now();
    let Err(partial) = started else {
        panic!("the second launch failed, so the start fails");
    };
    let failed_at = locked(&state.failed_at).expect("the second launch was refused");
    assert!(
        partial.value.is_none(),
        "no stop proof while the holder lives"
    );
    let failures = failures(&partial.error)
        .unwrap_or_else(|| panic!("containment expected: {:?}", partial.error));
    let Some(crate::error::CleanupFailure::Start(cause)) = failures.first() else {
        panic!("the start failure stays first: {failures:?}");
    };
    assert!(
        matches!(&**cause, crate::Error::Launch { instance, source, .. }
            if instance == "c01" && source.to_string() == INJECTED),
        "{cause:?}"
    );
    assert!(
        failures
            .iter()
            .any(|failure| matches!(failure, crate::error::CleanupFailure::DrainDeadline)),
        "the holder keeps the pipes open: {failures:?}"
    );
    let mut killed: Vec<String> = locked(&state.systemd)
        .iter()
        .filter_map(|call| match call {
            Systemd::Kill { scope, signal } if *signal == crate::os::KillSignal::Kill => {
                Some(scope.to_string())
            }
            _ => None,
        })
        .collect();
    killed.sort();
    assert_eq!(killed, ["togi-trial-t1-c00", "togi-trial-t1-c01"]);
    let rollback_time = returned.saturating_duration_since(failed_at);
    assert!(
        rollback_time <= teardown + secs(1),
        "the rollback is bounded by the teardown budget: {rollback_time:?}"
    );
    let holders = locked(&state.holders);
    assert!(
        holders.iter().all(Escaped::alive),
        "the detached holder survived the rollback"
    );
    holders.iter().for_each(Escaped::kill);
}

#[test]
fn real_process_systemctl_calls_are_bounded_and_a_timed_out_missing_scope_text_is_not_success() {
    use crate::id::ScopeName;
    use crate::os::KillSignal;
    let dir = crate::testing::tempdir();
    let script = dir.path().join("systemctl");
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'Unit togi-trial-t1.scope not loaded.'\nexec sleep 60\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let os =
        Linux::new(Proc::new("/proc".into(), ticks_per_second()), false).with_systemctl(&script);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
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
}
