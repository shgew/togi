use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read as _, Seek as _, Write as _};
use std::num::NonZeroU32;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use togi_journal::{FileError, Payload, ReadError, Recording, ShutdownReason};

use crate::error::{BuildFailure, Error, RunError, Unrunnable};
use crate::process::{self, Fleet};
use crate::suite::RunSpec;

const JOURNAL: &str = "events.jsonl";
const LOG: &str = "sim.log";
const DEAD_END_LINE: &str = "sim: dead end ";
const BINARY: &str = "togi-tool-sim";

/// The simulator command, run once per session as a subprocess with a state directory of its own. The bench
/// runs a private copy of the file it hashed, so the digest names the code every session ran even when the
/// original is rebuilt or replaced meanwhile.
#[derive(Clone, Debug)]
pub struct Simulator {
    binary: PathBuf,
    digest: String,
    _private: Arc<tempfile::TempDir>,
}

/// What one simulator process left behind.
#[derive(Debug)]
pub struct Session {
    pub dir: PathBuf,
    pub exit: i32,
    pub wall: Duration,
    pub events: Vec<togi_journal::Event>,
    /// Whether the process's log holds the simulator's dead-end line.
    pub log_dead_end: bool,
}

impl Session {
    pub fn journal_dead_end(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event.payload, Payload::DeadEnd(_)))
    }

    /// Whether a signal ended the session rather than the session itself: the process died of one, or its
    /// journal ends in a shutdown a signal asked for. Its outcome is not determined by its inputs.
    pub fn ended_by_signal(&self) -> bool {
        self.exit == -1
            || self.events.iter().any(|event| {
                matches!(&event.payload, Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal)
            })
    }
}

impl Simulator {
    /// The simulator at `explicit`, or the `togi-tool-sim` built from this tree, copied for this bench to run.
    pub fn locate(explicit: Option<&Path>) -> Result<Self, Error> {
        let original = match explicit {
            Some(path) => path.to_path_buf(),
            None => build()?,
        };
        let (bytes, mode) = File::open(&original)
            .and_then(|mut file| {
                let mode = file.metadata()?.permissions().mode();
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                Ok((bytes, mode))
            })
            .map_err(|source| Error::NoSimulator {
                path: original.clone(),
                source,
            })?;
        check_runnable(&bytes, mode, probe).map_err(|reason| Error::Unrunnable {
            path: original.clone(),
            reason,
        })?;
        let failed = |source| Error::WriteFile {
            what: "copy the simulator to",
            path: std::env::temp_dir(),
            source,
        };
        let private = tempfile::Builder::new()
            .prefix("togi-bench-sim-")
            .tempdir()
            .map_err(failed)?;
        fs::set_permissions(private.path(), fs::Permissions::from_mode(0o755)).map_err(failed)?;
        let binary = private.path().join(BINARY);
        File::options()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(&binary)
            .and_then(|mut file| file.write_all(&bytes))
            .map_err(failed)?;
        let copy = fs::read(&binary).map_err(failed)?;
        Ok(Self {
            digest: crate::suite::hex(&Sha256::digest(copy)),
            binary,
            _private: Arc::new(private),
        })
    }

    /// The SHA-256 of the simulator's bytes, in lower-case hex: the code the sessions ran.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Runs `spec` in a state directory below `root`, which this call creates, and reads what the process left:
    /// its journal, if it wrote one, and its log. A session that failed or stopped at a dead end or the boot cap
    /// is a result, not an error; a simulator that cannot start or outlives `limit` is an error.
    pub fn run(
        &self,
        fleet: &Fleet,
        root: &Path,
        spec: &RunSpec,
        max_boots: NonZeroU32,
        limit: Duration,
    ) -> Result<Session, RunError> {
        let launched = self.launch(fleet, root, spec, max_boots, limit)?;
        let events = recorded_events(Recording::read(&launched.dir.join(JOURNAL)))?;
        Ok(Session {
            dir: launched.dir,
            exit: launched.exit,
            wall: launched.wall,
            events,
            log_dead_end: launched.log_dead_end,
        })
    }

    /// Runs `spec` in the state directory below `root` that this call creates, as [`run`](Self::run) does, but
    /// reads no journal: a caller that resumes a copied state directory or digests the files reads them itself.
    pub(crate) fn launch(
        &self,
        fleet: &Fleet,
        root: &Path,
        spec: &RunSpec,
        max_boots: NonZeroU32,
        limit: Duration,
    ) -> Result<Launched, RunError> {
        let dir = root
            .join(&spec.scenario)
            .join(format!("{}-{}", spec.split, spec.seed));
        fs::create_dir_all(&dir).map_err(|source| RunError::Directory {
            path: dir.clone(),
            source,
        })?;
        let log_path = dir.join(LOG);
        let log = File::create(&log_path).map_err(|source| RunError::Directory {
            path: log_path.clone(),
            source,
        })?;
        let again = log.try_clone().map_err(|source| RunError::Directory {
            path: log_path.clone(),
            source,
        })?;
        let mut command = Command::new(&self.binary);
        command
            .arg("--seed")
            .arg(spec.seed.to_string())
            .arg("--state-dir")
            .arg(&dir)
            .arg("--max-boots")
            .arg(max_boots.to_string());
        if let Some(machine) = &spec.machine {
            command.arg("--machine").arg(&machine.path);
        }
        if spec.replay.is_some() {
            command.arg("--replay-facts");
        }
        command.stdin(Stdio::null()).stdout(log).stderr(again);
        let finished = process::run(fleet, &mut command, limit)?;
        let log_dead_end = File::open(&log_path)
            .and_then(holds_dead_end)
            .map_err(|source| RunError::Read {
                what: "log",
                source,
            })?;
        Ok(Launched {
            dir,
            exit: finished.exit,
            wall: finished.wall,
            log_dead_end,
        })
    }
}

/// What a simulator process left besides its journal.
#[derive(Debug)]
pub(crate) struct Launched {
    pub dir: PathBuf,
    pub exit: i32,
    pub wall: Duration,
    /// Whether the process's log holds the simulator's dead-end line.
    pub log_dead_end: bool,
}

/// Builds `togi-tool-sim` in the tree the bench runs in and returns the binary cargo reports.
/// Cargo's own progress goes to this process's stderr.
#[expect(
    clippy::disallowed_methods,
    reason = "the bench builds the simulator it benchmarks with cargo"
)]
fn build() -> Result<PathBuf, Error> {
    let output = cargo_build(None)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| Error::BuildSimulator(BuildFailure::Spawn(error)))?;
    if !output.status.success() {
        return Err(Error::BuildSimulator(BuildFailure::Status(output.status)));
    }
    built(&output.stdout)
}

/// Builds `togi-tool-sim` in `tree` as [`build`] does, with `target` as cargo's target directory when given,
/// in a process group of its own that `fleet` stops when it is cancelled.
pub(crate) fn build_in(
    fleet: &Fleet,
    tree: &Path,
    target: Option<&Path>,
) -> Result<PathBuf, Error> {
    let unkept = |error: io::Error| Error::BuildSimulator(BuildFailure::Messages(error));
    let mut messages = tempfile::tempfile().map_err(unkept)?;
    let mut command = cargo_build(target);
    command
        .current_dir(tree)
        .stdin(Stdio::null())
        .stdout(messages.try_clone().map_err(unkept)?)
        .stderr(Stdio::inherit());
    let finished = process::run(fleet, &mut command, Duration::MAX).map_err(|error| {
        Error::BuildSimulator(match error {
            RunError::Start(error) => BuildFailure::Spawn(error),
            error @ (RunError::Directory { .. }
            | RunError::Wait(_)
            | RunError::TimedOut { .. }
            | RunError::Read { .. }
            | RunError::Journal(_)
            | RunError::Measure(_)
            | RunError::Model(_)
            | RunError::Interrupted) => BuildFailure::Run(error),
        })
    })?;
    if finished.exit != 0 {
        return Err(Error::BuildSimulator(BuildFailure::Exit(finished.exit)));
    }
    let mut bytes = Vec::new();
    messages
        .rewind()
        .and_then(|()| messages.read_to_end(&mut bytes))
        .map_err(unkept)?;
    built(&bytes)
}

fn cargo_build(target: Option<&Path>) -> Command {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.args([
        "build",
        "--release",
        "--locked",
        "-p",
        BINARY,
        "--message-format=json-render-diagnostics",
    ]);
    if let Some(target) = target {
        command.arg("--target-dir").arg(target);
    }
    command
}

fn built(messages: &[u8]) -> Result<PathBuf, Error> {
    executable(messages).ok_or(Error::BuildSimulator(BuildFailure::NoExecutable))
}

/// The executable of the `togi-tool-sim` binary in cargo's JSON build messages.
fn executable(messages: &[u8]) -> Option<PathBuf> {
    messages
        .split(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter(|message| message["target"]["name"] == BINARY)
        .filter(|message| {
            message["target"]["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
        })
        .find_map(|message| message["executable"].as_str().map(PathBuf::from))
}

/// Refuses what `exec` would not load. Apple's `posix_spawnp`, which Rust's `Command` uses there, and `execvp`
/// hand a file the kernel rejects with ENOEXEC to `/bin/sh` as a script, so such a file would run as shell
/// code. The bench therefore accepts only an executable file that is:
/// - a binary in this host's format: ELF, or on Apple platforms a thin 64-bit little-endian Mach-O of type
///   `MH_EXECUTE` or a fat file;
/// - or a `#!` script whose first line ends within [`SHEBANG_LIMIT`] bytes and names an interpreter that
///   `interpreter` finds to be an executable binary in this host's format, as XNU allows no nested
///   interpreters; Linux would run one, but the bench refuses it on every platform alike.
///
/// The slices of a fat file are not inspected, so a fat file with no slice for this host, or another format
/// sharing the fat magic, still reaches the kernel.
fn check_runnable(
    bytes: &[u8],
    mode: u32,
    interpreter: impl FnOnce(&Path) -> io::Result<(Vec<u8>, u32)>,
) -> Result<(), Unrunnable> {
    if mode & 0o111 == 0 {
        return Err(Unrunnable::NotExecutable);
    }
    if host_binary(bytes) {
        return Ok(());
    }
    if !bytes.starts_with(b"#!") {
        return Err(Unrunnable::NotAProgram);
    }
    let head = &bytes[..bytes.len().min(SHEBANG_LIMIT)];
    let Some(end) = head.iter().position(|&byte| byte == b'\n') else {
        return Err(Unrunnable::LongShebang);
    };
    let path = head[2..end]
        .split(|&byte| byte == b' ' || byte == b'\t')
        .find(|token| !token.is_empty())
        .map(|token| Path::new(OsStr::from_bytes(token)))
        .ok_or(Unrunnable::NoInterpreter)?;
    let (prefix, mode) = interpreter(path).map_err(|error| Unrunnable::UnreadableInterpreter {
        interpreter: path.to_path_buf(),
        kind: error.kind(),
    })?;
    if mode & 0o111 == 0 {
        return Err(Unrunnable::InterpreterNotExecutable {
            interpreter: path.to_path_buf(),
        });
    }
    if !host_binary(&prefix) {
        return Err(Unrunnable::InterpreterNotAProgram {
            interpreter: path.to_path_buf(),
        });
    }
    Ok(())
}

/// XNU's `IMG_SHSIZE`: how far the kernel reads for a `#!` line.
const SHEBANG_LIMIT: usize = 512;

/// How many leading bytes [`host_binary`] reads.
const BINARY_PREFIX: u64 = 16;

/// The leading bytes and mode of the file at `path`, following symlinks.
fn probe(path: &Path) -> io::Result<(Vec<u8>, u32)> {
    let file = File::open(path)?;
    let mode = file.metadata()?.permissions().mode();
    let mut prefix = Vec::new();
    file.take(BINARY_PREFIX).read_to_end(&mut prefix)?;
    Ok((prefix, mode))
}

#[cfg(target_vendor = "apple")]
fn host_binary(bytes: &[u8]) -> bool {
    const MH_EXECUTE: u32 = 2;
    if bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]) {
        return bytes
            .get(12..16)
            .is_some_and(|filetype| filetype == MH_EXECUTE.to_le_bytes());
    }
    bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe])
}

#[cfg(not(target_vendor = "apple"))]
fn host_binary(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x7fELF")
}

fn recorded_events(
    recording: Result<Recording, FileError>,
) -> Result<Vec<togi_journal::Event>, RunError> {
    match recording {
        Ok(recording) => Ok(recording.into_events()),
        Err(FileError {
            source: ReadError::Io(error),
            ..
        }) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(RunError::Journal(error)),
    }
}

/// How many bytes of a log [`holds_dead_end`] reads at a time.
const SCAN_CHUNK: usize = 64 * 1024;

/// Whether `log` holds [`DEAD_END_LINE`], read through one buffer of [`SCAN_CHUNK`] bytes whatever the log's size
/// or the length of its lines. The buffer keeps the last bytes of each chunk, too few to hold the marker, ahead of
/// the next, so a marker split across reads is still found. The bytes are searched as they are: the marker is
/// ASCII, and invalid UTF-8 around it never absorbs an ASCII byte.
fn holds_dead_end(mut log: impl io::Read) -> io::Result<bool> {
    let marker = DEAD_END_LINE.as_bytes();
    let mut buf = vec![0; SCAN_CHUNK];
    let mut kept = 0;
    loop {
        let read = match log.read(&mut buf[kept..]) {
            Ok(0) => return Ok(false),
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let filled = kept + read;
        if buf[..filled]
            .windows(marker.len())
            .any(|window| window == marker)
        {
            return Ok(true);
        }
        kept = filled.min(marker.len() - 1);
        buf.copy_within(filled - kept..filled, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_journal_has_no_events_but_a_damaged_journal_is_an_error() {
        let fs = togi_journal::MemFs::new();
        let path = Path::new("/runs/events.jsonl");
        assert!(
            recorded_events(Recording::read_in(&fs, path))
                .unwrap()
                .is_empty()
        );
        fs.put(path, b"broken\n".to_vec());
        let error = recorded_events(Recording::read_in(&fs, path)).unwrap_err();
        assert!(matches!(error, RunError::Journal(_)));
        assert_eq!(
            togi_machine::error_chain(&error),
            "read the run journal: /runs/events.jsonl: first line is not a complete header of this build; either another build's journal or a header torn by a crash; see docs/howto.md#go-to-rust-cutover"
        );
    }

    /// Hands out at most `step` bytes of `inner` per read and remembers the largest buffer it was offered.
    struct Offered<R> {
        inner: R,
        step: usize,
        largest: usize,
    }

    impl<R: io::Read> io::Read for Offered<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.largest = self.largest.max(buf.len());
            let len = buf.len().min(self.step);
            self.inner.read(&mut buf[..len])
        }
    }

    fn offered(log: impl io::Read, step: usize) -> (bool, usize) {
        let mut reader = Offered {
            inner: log,
            step,
            largest: 0,
        };
        let found = holds_dead_end(&mut reader).unwrap();
        (found, reader.largest)
    }

    /// Scans a single 16 MiB line of `x` followed by `tail`, checking the scanner never offers more than its buffer.
    fn after_a_large_line(tail: &[u8]) -> bool {
        let log = io::repeat(b'x').take(16 << 20).chain(tail);
        let (found, largest) = offered(log, usize::MAX);
        assert!(largest <= SCAN_CHUNK, "offered {largest} bytes");
        found
    }

    #[test]
    fn a_marker_after_a_log_far_larger_than_the_scan_buffer_is_found() {
        assert!(after_a_large_line(b"\xff\xfe sim: dead end 3"));
    }

    #[test]
    fn a_near_miss_after_a_log_far_larger_than_the_scan_buffer_is_not_found() {
        assert!(!after_a_large_line(b"\xff\xfe sim: dead en"));
    }

    #[test]
    fn a_log_far_larger_than_the_scan_buffer_without_the_marker_is_read_through_it() {
        assert!(!after_a_large_line(b""));
    }

    #[test]
    fn a_marker_split_across_reads_is_found() {
        let marker = DEAD_END_LINE.as_bytes();
        for step in 1..=marker.len() + 1 {
            for lead in 0..step {
                let mut log = vec![b'x'; lead];
                log.extend_from_slice(marker);
                log.extend_from_slice(b"7\n");
                assert!(offered(log.as_slice(), step).0, "step {step} lead {lead}");
            }
        }
    }

    const MACH_O_EXECUTE: [u8; 16] = [
        0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 0x01, 0, 0, 0, 0, 0x02, 0, 0, 0,
    ];
    const ELF: [u8; 6] = *b"\x7fELF\x02\x01";

    #[cfg(target_vendor = "apple")]
    const HOST: &[u8] = &MACH_O_EXECUTE;
    #[cfg(target_vendor = "apple")]
    const FOREIGN: &[u8] = &ELF;
    #[cfg(not(target_vendor = "apple"))]
    const HOST: &[u8] = &ELF;
    #[cfg(not(target_vendor = "apple"))]
    const FOREIGN: &[u8] = &MACH_O_EXECUTE;

    fn no_interpreter(_: &Path) -> io::Result<(Vec<u8>, u32)> {
        panic!("no interpreter should be read")
    }

    #[test_case::test_case(&b"#!/bin/sh\n"[..]; "b bin sh n")]
    #[test_case::test_case(b"#! /bin/sh -e\necho hi\n"; "b bin sh e necho hi n")]
    #[test_case::test_case(b"#!\t/bin/sh\n"; "b t bin sh n")]
    fn a_host_binary_or_a_script_with_a_host_binary_interpreter_passes(case: &[u8]) {
        let script = case;
        assert_eq!(check_runnable(HOST, 0o755, no_interpreter), Ok(()));

        assert_eq!(
            check_runnable(script, 0o755, |path| {
                assert_eq!(path, Path::new("/bin/sh"));
                Ok((HOST.to_vec(), 0o755))
            }),
            Ok(()),
            "{script:?}"
        );
    }

    #[test_case::test_case(&b"not a program"[..]; "b not a program")]
    #[test_case::test_case(b"echo hi\n"; "b echo hi n")]
    #[test_case::test_case(b""; "b")]
    #[test_case::test_case(b"\x7fEL"; "b x7fEL")]
    #[test_case::test_case(b" #!/bin/sh"; "b bin sh")]
    #[test_case::test_case(FOREIGN; "FOREIGN")]
    fn a_file_the_kernel_would_hand_to_a_shell_is_refused(case: &[u8]) {
        let bytes = case;

        assert_eq!(
            check_runnable(bytes, 0o755, no_interpreter),
            Err(Unrunnable::NotAProgram),
            "{bytes:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[derive(Clone, Copy)]
    enum MachOInput {
        DynamicLibrary,
        BigEndian,
        Truncated,
    }

    #[cfg(target_os = "macos")]
    #[test_case::test_case(MachOInput::DynamicLibrary; "DynamicLibrary")]
    #[test_case::test_case(MachOInput::BigEndian; "BigEndian")]
    #[test_case::test_case(MachOInput::Truncated; "Truncated")]
    fn a_mach_o_that_is_not_an_executable_is_refused(scenario: MachOInput) {
        let mut dylib = MACH_O_EXECUTE;
        dylib[12] = 0x06;
        let mut big_endian = MACH_O_EXECUTE;
        big_endian[..4].copy_from_slice(&[0xfe, 0xed, 0xfa, 0xcf]);
        let bytes: &[u8] = match scenario {
            MachOInput::DynamicLibrary => &dylib[..],
            MachOInput::BigEndian => &big_endian,
            MachOInput::Truncated => &MACH_O_EXECUTE[..12],
        };

        assert_eq!(
            check_runnable(bytes, 0o755, no_interpreter),
            Err(Unrunnable::NotAProgram),
            "{bytes:?}"
        );
    }

    #[test_case::test_case(&b"#!\n"[..]; "b n")]
    #[test_case::test_case(b"#!   \necho hi\n"; "b necho hi n")]
    #[test_case::test_case(b"#!\t \t\n"; "b t t n")]
    fn a_script_naming_no_interpreter_is_refused(case: &[u8]) {
        let script = case;

        assert_eq!(
            check_runnable(script, 0o755, no_interpreter),
            Err(Unrunnable::NoInterpreter),
            "{script:?}"
        );
    }

    #[derive(Clone, Copy)]
    enum ShebangLength {
        Unterminated,
        TooLong,
    }

    #[test_case::test_case(ShebangLength::Unterminated; "Unterminated")]
    #[test_case::test_case(ShebangLength::TooLong; "TooLong")]
    fn a_script_whose_first_line_outruns_the_kernel_is_refused(scenario: ShebangLength) {
        let mut long = b"#!/bin/sh ".to_vec();
        long.resize(SHEBANG_LIMIT, b'x');
        long.push(b'\n');
        let script: &[u8] = match scenario {
            ShebangLength::Unterminated => &b"#!/bin/sh"[..],
            ShebangLength::TooLong => &long,
        };

        assert_eq!(
            check_runnable(script, 0o755, no_interpreter),
            Err(Unrunnable::LongShebang)
        );
    }

    #[test_case::test_case(&b"#!/bin/sh\n"[..]; "b bin sh n")]
    #[test_case::test_case(FOREIGN; "FOREIGN")]
    #[test_case::test_case(b"echo hi\n"; "b echo hi n")]
    fn a_script_whose_interpreter_is_not_a_host_binary_is_refused(case: &[u8]) {
        let prefix = case;
        let interpreter = PathBuf::from("/opt/run");

        assert_eq!(
            check_runnable(b"#!/opt/run\n", 0o755, |_| Ok((prefix.to_vec(), 0o755))),
            Err(Unrunnable::InterpreterNotAProgram {
                interpreter: interpreter.clone()
            }),
            "{prefix:?}"
        );

        assert_eq!(
            check_runnable(b"#!/opt/run\n", 0o755, |_| Ok((HOST.to_vec(), 0o644))),
            Err(Unrunnable::InterpreterNotExecutable { interpreter })
        );
    }

    #[test]
    fn a_script_whose_interpreter_is_missing_is_refused() {
        assert_eq!(
            check_runnable(b"#!/opt/missing\n", 0o755, |_| Err(
                ErrorKind::NotFound.into()
            )),
            Err(Unrunnable::UnreadableInterpreter {
                interpreter: PathBuf::from("/opt/missing"),
                kind: ErrorKind::NotFound,
            })
        );
    }

    #[test]
    fn real_fs_the_probe_follows_symlinks_to_the_interpreter() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("interpreter");
        fs::write(&target, HOST).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let mut script = b"#!".to_vec();
        script.extend_from_slice(link.as_os_str().as_bytes());
        script.push(b'\n');
        assert_eq!(check_runnable(&script, 0o755, probe), Ok(()));
        assert_eq!(
            check_runnable(b"#!/nonexistent/interpreter\n", 0o755, probe),
            Err(Unrunnable::UnreadableInterpreter {
                interpreter: PathBuf::from("/nonexistent/interpreter"),
                kind: ErrorKind::NotFound,
            })
        );
    }

    #[test]
    fn a_file_without_an_execute_bit_is_refused_whatever_it_holds() {
        assert_eq!(
            check_runnable(b"#!/bin/sh\n", 0o644, no_interpreter),
            Err(Unrunnable::NotExecutable)
        );
        assert_eq!(
            check_runnable(HOST, 0o600, no_interpreter),
            Err(Unrunnable::NotExecutable)
        );
    }
}
