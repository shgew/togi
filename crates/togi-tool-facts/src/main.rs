use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use togi_tool_facts::generate;

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
    match generate(&cli.state_dir, &cli.output) {
        Ok(count) => match writeln!(std::io::stdout(), "extracted {count} facts") {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "togi-tool-facts: write the count: {error}"
                );
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "togi-tool-facts: {error}");
            ExitCode::FAILURE
        }
    }
}
