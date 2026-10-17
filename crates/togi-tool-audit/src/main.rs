use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use togi_tool_audit::{Completion, DirectoryAudit, Finding, audit_directory, audit_with};

const USAGE: u8 = 2;

/// The command's journal reader and output streams; production supplies the durable disk reader, which the command
/// tells how far to hold each directory to its end.
pub(crate) struct Io<'a> {
    pub(crate) inspect: &'a dyn Fn(&Path, Completion) -> DirectoryAudit,
    pub(crate) out: &'a mut dyn io::Write,
    pub(crate) err: &'a mut dyn io::Write,
}

/// Audit the current and archived journals of the given state directories.
///
/// Each directory is audited as a run that may still be in progress. The audit holds each directory's writer lock,
/// so audit a copy of one a tuner may use. Violations are JSON Lines on stdout; the summary is on stderr. The bench
/// audits every run it simulates itself.
#[derive(Debug, Parser)]
#[command(name = "togi-tool-audit", version = togi_version::version())]
struct Cli {
    /// State directories to audit.
    #[arg(value_name = "STATE-DIR", required = true)]
    dirs: Vec<PathBuf>,
}

fn main() -> ExitCode {
    entry(
        std::env::args_os(),
        &mut Io {
            inspect: &audit_directory,
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
        Ok(cli) => {
            let inspect = io.inspect;
            report(
                &audit_with(&cli.dirs, |dir| inspect(dir, Completion::InProgress)),
                io,
            )
        }
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

fn report(audited: &togi_tool_audit::Report, io: &mut Io<'_>) -> ExitCode {
    if let Err(error) = write_findings(&audited.findings, io.out) {
        let _ = writeln!(io.err, "togi-tool-audit: write the report: {error}");
        return ExitCode::FAILURE;
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
