//! `release`: see the library documentation.

use std::error::Error;
use std::io::{self, Write as _};
use std::process::ExitCode;

use jiff::Timestamp;
use togi_tool_release::cli::{self, Command, Parsed, USAGE};
use togi_tool_release::fragments::check_fragments;
use togi_tool_release::gates::{GitHubGates, SystemClock};
use togi_tool_release::git::ProcessGit;
use togi_tool_release::github::{self, HttpTransport};
use togi_tool_release::release::{Mode, Runner};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let mut stderr = io::stderr().lock();
    // Nothing is left to report a failed write to standard error to.
    match cli::parse(&args) {
        Parsed::Help => {
            let _ = write!(stderr, "{USAGE}");
            ExitCode::SUCCESS
        }
        Parsed::Usage(message) => {
            if let Some(message) = message {
                let _ = writeln!(stderr, "{message}");
            }
            let _ = write!(stderr, "{USAGE}");
            ExitCode::from(2)
        }
        Parsed::Run(command) => match run(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(stderr, "release: {}", togi_tool_release::chain(&*error));
                ExitCode::FAILURE
            }
        },
    }
}

fn run(command: Command) -> Result<(), Box<dyn Error>> {
    if let Command::Check(dir) = command {
        return Ok(check_fragments(&dir)?);
    }
    let mut git = ProcessGit::new(".");
    let mut out = io::stdout().lock();
    let mut runner = Runner {
        git: &mut git,
        today: Timestamp::now(),
        out: &mut out,
    };
    if let Command::Preview = command {
        return Ok(runner.release(Mode::Preview)?);
    }
    let (mut api, repository) =
        github::from_env(|key| std::env::var(key).ok(), Box::new(HttpTransport))?;
    if let Command::Publish = command {
        return Ok(runner.publish(&mut api, &repository)?);
    }
    let mut gates = GitHubGates {
        api: &mut api,
        repository: &repository,
        clock: SystemClock,
    };
    Ok(runner.release(Mode::Commit(&mut gates))?)
}
