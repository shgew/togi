use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use sha2::{Digest, Sha256};
use togi_tool_sim::scripted::{CONTRACT_VERSION, ScriptedCase};

use crate::error::{BuildFailure, Error, RunError, ScriptedError, Unrunnable};
use crate::process::{self, Fleet};

const LOG: &str = "sim.log";
const BINARY: &str = "togi-tool-sim";

/// The simulator command, run once per session as a subprocess with a state directory of its own. The bench
/// runs a private copy of the file it hashed, so the digest names the code every session ran even when the
/// original is rebuilt or replaced meanwhile.
#[derive(Debug)]
pub(crate) struct Simulator {
    binary: PathBuf,
    digest: String,
    _private: tempfile::TempDir,
}

impl Simulator {
    /// Copy the supplied or freshly built simulator before running it.
    pub(crate) fn locate(original: &Path) -> Result<Self, Error> {
        let (bytes, mode) = File::open(original)
            .and_then(|mut file| {
                let mode = file.metadata()?.permissions().mode();
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                Ok((bytes, mode))
            })
            .map_err(|source| Error::NoSimulator {
                path: original.to_path_buf(),
                source,
            })?;
        check_runnable(&bytes, mode, probe).map_err(|reason| Error::Unrunnable {
            path: original.to_path_buf(),
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
        Ok(Self {
            digest: crate::hex(&Sha256::digest(bytes)),
            binary,
            _private: private,
        })
    }

    /// The SHA-256 of the simulator's bytes, in lower-case hex: the code the sessions ran.
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    /// Probes the private copy, so subsequent cases and the handshake use the same bytes.
    pub(crate) fn check_scripted_contract(
        &self,
        fleet: &Fleet,
        limit: Duration,
    ) -> Result<(), ScriptedError> {
        let read = |source| RunError::Read {
            what: "scripted-contract output",
            source,
        };
        let mut stdout = tempfile::tempfile().map_err(read)?;
        let mut command = Command::new(&self.binary);
        command
            .arg("scripted-contract")
            .stdin(Stdio::null())
            .stdout(stdout.try_clone().map_err(read)?)
            .stderr(Stdio::null());
        let finished = process::run(fleet, &mut command, limit)?;
        if finished.exit != 0 {
            return Err(ScriptedError::Exit {
                exit: finished.exit,
            });
        }
        stdout.rewind().map_err(read)?;
        let mut answer = Vec::new();
        stdout.take(128).read_to_end(&mut answer).map_err(read)?;
        if answer.strip_suffix(b"\n") != Some(CONTRACT_VERSION.as_bytes()) {
            return Err(ScriptedError::Version {
                answer: String::from_utf8_lossy(&answer).into_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn launch_scripted(
        &self,
        fleet: &Fleet,
        dir: &Path,
        case: ScriptedCase,
        limit: Duration,
    ) -> Result<process::Finished, ScriptedError> {
        let mut command = Command::new(&self.binary);
        command
            .args([
                "scripted",
                "--case",
                case.name(),
                "--contract",
                CONTRACT_VERSION,
            ])
            .arg("--state-dir")
            .arg(dir)
            .arg("--max-boots")
            .arg(case.max_boots().to_string());
        let finished = launch_logged(fleet, &mut command, dir, limit)?;
        if finished.exit != 0 {
            return Err(ScriptedError::Exit {
                exit: finished.exit,
            });
        }
        Ok(finished)
    }
}

fn launch_logged(
    fleet: &Fleet,
    command: &mut Command,
    dir: &Path,
    limit: Duration,
) -> Result<process::Finished, RunError> {
    fs::create_dir_all(dir).map_err(|source| RunError::Directory {
        path: dir.to_path_buf(),
        source,
    })?;
    let log_path = dir.join(LOG);
    let log = File::create(&log_path).map_err(|source| RunError::Directory {
        path: log_path.clone(),
        source,
    })?;
    let again = log.try_clone().map_err(|source| RunError::Directory {
        path: log_path,
        source,
    })?;
    command.stdin(Stdio::null()).stdout(log).stderr(again);
    process::run(fleet, command, limit)
}

/// Builds `togi-tool-sim` in `tree`, with the given target directory if supplied, in a
/// process group of its own that `fleet` stops when cancelled.
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

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;

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
