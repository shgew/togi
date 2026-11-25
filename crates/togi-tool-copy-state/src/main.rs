use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// Copy the live journal and its archived journals and markers for inspection and replay.
/// Trial directories are left out. Take the copy while no trial runs.
#[derive(Debug, Parser)]
#[command(name = "togi-tool-copy-state")]
struct Cli {
    /// The state directory to copy, only read.
    src: PathBuf,
    /// The directory to copy into, created with its missing ancestors; files of the same names are replaced.
    dst: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match togi_statecopy::copy(&cli.src, &cli.dst) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "togi-tool-copy-state: {}",
                togi_machine::error_chain(&error)
            );
            ExitCode::FAILURE
        }
    }
}
