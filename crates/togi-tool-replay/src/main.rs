use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser;
use togi_tool_replay::{Cli, run};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli, &mut std::io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "togi-tool-replay: {error}");
            ExitCode::FAILURE
        }
    }
}
