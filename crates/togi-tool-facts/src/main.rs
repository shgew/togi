use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use togi_tool_facts::{GenerateError, Generated, generate};

/// Extract the privacy-safe trial facts of a copied state directory. Work on a copy: reading takes the
/// directory's writer lock.
#[derive(Debug, Parser)]
#[command(name = "togi-tool-facts")]
struct Cli {
    /// The copied state directory to read.
    state_dir: PathBuf,
    /// The gzip JSON Lines extract to write, replaced atomically.
    output: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    run(
        &cli,
        |state, output| generate(state, output),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

fn run(
    cli: &Cli,
    generate: impl FnOnce(&std::path::Path, &std::path::Path) -> Result<Generated, GenerateError>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> ExitCode {
    match generate(&cli.state_dir, &cli.output) {
        Ok(generated) => {
            for skipped in &generated.skipped {
                let _ = writeln!(stderr, "togi-tool-facts: {skipped}");
            }
            match writeln!(stdout, "extracted {} facts", generated.count) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    let _ = writeln!(stderr, "togi-tool-facts: write the count: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => {
            let _ = writeln!(stderr, "togi-tool-facts: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[path = "../tests/facts/fixture.rs"]
mod fixture;

#[cfg(test)]
#[path = "../tests/facts/cli.rs"]
mod tests;
