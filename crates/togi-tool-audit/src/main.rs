use std::io::{self, Write as _};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use togi_tool_audit::{
    Evidence, Finding, INTERRUPTED, Interrupt, JournalSkip, Record, RecordError, Sweep,
    audit_directory, audit_with, read_records,
};

const USAGE: u8 = 2;

type Inspection = (Vec<Finding>, usize, Vec<JournalSkip>);

/// The command's journal/record readers and output streams; production supplies durable disk readers.
pub(crate) struct Io<'a> {
    pub(crate) inspect: &'a dyn Fn(&Path, bool) -> Inspection,
    pub(crate) records: &'a dyn Fn(&Path) -> Result<Vec<Record>, RecordError>,
    pub(crate) out: &'a mut dyn io::Write,
    pub(crate) err: &'a mut dyn io::Write,
}

/// Audit the journals of a simulated sweep, or of the given state directories.
///
/// Without state directories, runs the bench over every scenario of the suite for seeds 1 to --seeds, keeps each
/// run's state directory and audits them all; the evidence is removed afterwards unless --keep names a directory. With state directories, reads their current and archived journals;
/// the audit holds each directory's writer lock, so audit a copy of one a tuner may use. Violations are JSON Lines
/// on stdout; the summary and the evidence paths are on stderr.
#[derive(Debug, Parser)]
#[command(name = "togi-tool-audit", version = togi_version::version())]
struct Cli {
    /// Seeds per scenario, numbered from 1.
    #[arg(long, value_name = "N", default_value = "200")]
    seeds: NonZeroU32,
    /// The most bench simulators at once; default the available parallelism.
    #[arg(long, value_name = "N")]
    jobs: Option<NonZeroUsize>,
    /// Retain the sweep evidence under DIR; default a temporary directory removed after the audit.
    #[arg(long, value_name = "DIR")]
    keep: Option<PathBuf>,
    /// The scenario suite; machine paths are relative to it.
    #[arg(long, value_name = "FILE", default_value = "tools/bench/suite.json")]
    suite: PathBuf,
    /// The bench's hang safety limit per simulator, in seconds; sessions are bounded by simulated boots.
    #[arg(long, value_name = "SECONDS", default_value = "1800")]
    timeout: NonZeroU64,
    /// The bench command; default togi-tool-bench beside this executable.
    #[arg(long, value_name = "FILE")]
    bench: Option<PathBuf>,
    /// The simulator command the bench runs; default the bench builds togi-tool-sim.
    #[arg(long, value_name = "FILE")]
    sim: Option<PathBuf>,
    /// Bench run records for the bench-status and wall-time checks.
    #[arg(long, value_name = "FILE", requires = "root")]
    records: Option<PathBuf>,
    /// The bench run root holding SCENARIO/SPLIT-SEED; requires --records.
    #[arg(long, value_name = "DIR", requires = "records")]
    root: Option<PathBuf>,
    /// State directories to audit instead of a sweep.
    #[arg(value_name = "STATE-DIR")]
    dirs: Vec<PathBuf>,
}

fn main() -> ExitCode {
    entry(
        std::env::args_os(),
        &mut Io {
            inspect: &audit_directory,
            records: &read_records,
            out: &mut io::stdout(),
            err: &mut io::stderr(),
        },
    )
}

pub(crate) fn entry(
    args: impl IntoIterator<Item = impl Into<std::ffi::OsString> + Clone>,
    io: &mut Io<'_>,
) -> ExitCode {
    match Cli::try_parse_from(args) {
        Ok(cli) => run(cli, io),
        Err(error) => {
            if error.use_stderr() {
                let _ = write!(io.err, "{}", error.render());
                ExitCode::from(USAGE)
            } else {
                let _ = write!(io.out, "{}", error.render());
                ExitCode::SUCCESS
            }
        }
    }
}

fn fail(error: &dyn std::fmt::Display, err: &mut dyn io::Write) -> ExitCode {
    let _ = writeln!(err, "togi-tool-audit: {error}");
    ExitCode::FAILURE
}

fn interrupted(err: &mut dyn io::Write) -> ExitCode {
    let _ = writeln!(err, "togi-tool-audit: interrupted");
    ExitCode::from(INTERRUPTED)
}

fn run(cli: Cli, io: &mut Io<'_>) -> ExitCode {
    let mut scratch = None;
    let mut watching = None;
    let (records, root) = match (&cli.records, &cli.root) {
        (Some(records), Some(root)) => (records.clone(), root.clone()),
        _ if cli.dirs.is_empty() => {
            let jobs = cli
                .jobs
                .or_else(|| std::thread::available_parallelism().ok())
                .unwrap_or(NonZeroUsize::MIN);
            let interrupt = match Interrupt::watch() {
                Ok(interrupt) => interrupt,
                Err(error) => return fail(&format!("watch for signals: {error}"), io.err),
            };
            let sweep = Sweep {
                suite: &cli.suite,
                seeds: NonZeroU64::from(cli.seeds),
                jobs: jobs.get(),
                timeout_s: cli.timeout.get(),
                bench: cli.bench.as_deref(),
                sim: cli.sim.as_deref(),
                keep: cli.keep.as_deref(),
                interrupt: &interrupt,
            };
            match sweep.run() {
                Ok(Evidence {
                    scratch: held,
                    dir,
                    records,
                    root,
                }) => {
                    if !interrupt.audit_started(held.as_ref().map(|_| dir)) {
                        return interrupted(io.err);
                    }
                    scratch = held;
                    watching = Some(interrupt);
                    (records, root)
                }
                Err(_) if interrupt.signalled() => return interrupted(io.err),
                Err(error) => return fail(&error, io.err),
            }
        }
        _ => return audit_only(&cli.dirs, io),
    };
    let code = audit_records(&records, &root, cli.dirs, watching.as_deref(), io);
    let held = watching.as_deref().map(Interrupt::hold);
    drop(scratch);
    drop(held);
    code
}

fn audit_only(dirs: &[PathBuf], io: &mut Io<'_>) -> ExitCode {
    report(audit_with(dirs, &[], Path::new(""), || (), io.inspect), io)
}

fn audit_records(
    records: &Path,
    root: &Path,
    mut dirs: Vec<PathBuf>,
    interrupt: Option<&Interrupt>,
    io: &mut Io<'_>,
) -> ExitCode {
    let records = match (io.records)(records) {
        Ok(records) => records,
        Err(error) => return fail(&error, io.err),
    };
    if dirs.is_empty() {
        dirs = records
            .iter()
            .map(|record| togi_tool_audit::run_directory(root, record))
            .collect();
    }
    report(
        audit_with(
            &dirs,
            &records,
            root,
            || interrupt.map(Interrupt::hold),
            io.inspect,
        ),
        io,
    )
}

fn report(audited: io::Result<togi_tool_audit::Report>, io: &mut Io<'_>) -> ExitCode {
    let audited = match audited {
        Ok(audited) => audited,
        Err(error) => return fail(&error, io.err),
    };
    if let Err(error) = write_findings(&audited.findings, io.out) {
        return fail(&format!("write the report: {error}"), io.err);
    }
    for skip in &audited.skipped {
        let _ = writeln!(
            io.err,
            "togi-tool-audit: {:?} {}: {}",
            skip.directory.to_string_lossy(),
            skip.journal,
            skip.skipped
        );
    }
    let _ = writeln!(
        io.err,
        "togi-tool-audit: directories={} journals={} violations={}",
        audited.directories,
        audited.journals,
        audited.findings.len()
    );
    if audited.findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn write_findings(findings: &[Finding], out: &mut dyn io::Write) -> io::Result<()> {
    let mut out = io::BufWriter::new(out);
    for finding in findings {
        serde_json::to_writer(&mut out, finding)?;
        out.write_all(b"\n")?;
    }
    out.flush()
}
