//! The two programs the tool runs: `git diff` for the changed lines and `rustc` for the host.

use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus};

use crate::Error;

/// Why a program the tool runs failed.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("start {program}")]
    Start {
        program: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{program} exited with {status}: {stderr}")]
    Exited {
        program: &'static str,
        status: ExitStatus,
        stderr: String,
    },
}

/// The standard output of `program args` run in `dir`.
pub fn output(program: &'static str, args: &[&str], dir: &Path) -> Result<Vec<u8>, ProcessError> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the coverage tool reads git's diff and rustc's host description"
    )]
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|source| ProcessError::Start { program, source })?;
    if !output.status.success() {
        return Err(ProcessError::Exited {
            program,
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(output.stdout)
}

/// The diff of the Rust files in `dir`'s working tree against its merge base with `base`, with no
/// context and no inter-hunk context whatever the Git configuration says.
pub fn rust_diff(dir: &Path, base: &str) -> Result<Vec<u8>, Error> {
    output(
        "git",
        &[
            "diff",
            "-U0",
            "--inter-hunk-context=0",
            "--no-color",
            "--no-ext-diff",
            "--no-prefix",
            "--merge-base",
            base,
            "--",
            "*.rs",
        ],
        dir,
    )
    .map_err(|source| Error::Diff {
        base: base.to_owned(),
        source,
    })
}
