use std::io::{self, Write as _};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use togi_tool_audit::{Evidence, Finding, Sweep, audit_directories, read_records};

const USAGE: u8 = 2;

/// Audit the journals of a simulated sweep, or of the given state directories.
///
/// Without state directories, runs the bench over every scenario of the suite for seeds 1 to --seeds, keeps each
/// run's state directory and audits them all. With state directories, reads their current and archived journals;
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
    /// Retain the sweep evidence under DIR; default a new temporary directory.
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
    /// Bench run records for the termination and wall-time checks.
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
    match Cli::try_parse() {
        Ok(cli) => run(cli),
        Err(error) => {
            if error.use_stderr() {
                let _ = write!(io::stderr(), "{}", error.render());
                ExitCode::from(USAGE)
            } else {
                let _ = write!(io::stdout(), "{}", error.render());
                ExitCode::SUCCESS
            }
        }
    }
}

fn fail(error: &dyn std::fmt::Display) -> ExitCode {
    let _ = writeln!(io::stderr(), "togi-tool-audit: {error}");
    ExitCode::FAILURE
}

fn run(cli: Cli) -> ExitCode {
    let (records, root) = match (&cli.records, &cli.root) {
        (Some(records), Some(root)) => (records.clone(), root.clone()),
        _ if cli.dirs.is_empty() => {
            let jobs = cli
                .jobs
                .or_else(|| std::thread::available_parallelism().ok())
                .unwrap_or(NonZeroUsize::MIN);
            let sweep = Sweep {
                suite: &cli.suite,
                seeds: NonZeroU64::from(cli.seeds),
                jobs: jobs.get(),
                timeout_s: cli.timeout.get(),
                bench: cli.bench.as_deref(),
                sim: cli.sim.as_deref(),
                keep: cli.keep.as_deref(),
            };
            match sweep.run() {
                Ok(Evidence { records, root, .. }) => (records, root),
                Err(error) => return fail(&error),
            }
        }
        _ => return audit_only(&cli.dirs),
    };
    audit_records(&records, &root, cli.dirs)
}

fn audit_only(dirs: &[PathBuf]) -> ExitCode {
    report(audit_directories(dirs, &[], Path::new("")))
}

fn audit_records(records: &Path, root: &Path, mut dirs: Vec<PathBuf>) -> ExitCode {
    let records = match read_records(records) {
        Ok(records) => records,
        Err(error) => return fail(&error),
    };
    if dirs.is_empty() {
        dirs = records
            .iter()
            .map(|record| togi_tool_audit::run_directory(root, record))
            .collect();
    }
    report(audit_directories(&dirs, &records, root))
}

fn report(audited: io::Result<togi_tool_audit::Report>) -> ExitCode {
    let audited = match audited {
        Ok(audited) => audited,
        Err(error) => return fail(&error),
    };
    if let Err(error) = write_findings(&audited.findings) {
        return fail(&format!("write the report: {error}"));
    }
    let _ = writeln!(
        io::stderr(),
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

fn write_findings(findings: &[Finding]) -> io::Result<()> {
    let mut out = io::BufWriter::new(io::stdout().lock());
    for finding in findings {
        serde_json::to_writer(&mut out, finding)?;
        out.write_all(b"\n")?;
    }
    out.flush()
}
