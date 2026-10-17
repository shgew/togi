//! `cover`: see the library documentation.

use std::io::{self, Write as _};
use std::process::ExitCode;

use togi_tool_cover::cli::{self, Command, USAGE};
use togi_tool_cover::{Error, chain, report};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let mut stderr = io::stderr().lock();
    // Nothing is left to report a failed write to standard error to.
    match run(&args) {
        Ok(Command::Help) => {
            let _ = write!(stderr, "{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Command::Report(_)) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(stderr, "cover: {}", chain(&error));
            if matches!(
                error,
                Error::Usage | Error::UnknownFlag(_) | Error::MissingValue(_)
            ) {
                let _ = write!(stderr, "{USAGE}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<Command, Error> {
    let command = cli::parse(args)?;
    if let Command::Report(options) = &command {
        let root = std::env::current_dir().map_err(Error::WorkingDirectory)?;
        report::run(options, &root, &mut io::stdout().lock())?;
    }
    Ok(command)
}
