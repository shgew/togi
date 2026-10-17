//! `release`: see the library documentation.

use std::error::Error;
use std::io::{self, Write};
use std::process::ExitCode;

use jiff::Timestamp;
use togi_tool_release::cli::{self, Command, Parsed, USAGE};
use togi_tool_release::fragments::check_fragments;
use togi_tool_release::gates::{GitHubGates, SystemClock};
use togi_tool_release::git::ProcessGit;
use togi_tool_release::github::{self, GitHub, HttpTransport, Repository};
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
    let mut git = ProcessGit::new(".");
    let mut out = io::stdout().lock();
    match command {
        Command::Check(dir) => Ok(check_fragments(&dir)?),
        Command::Preview => Ok(runner(&mut git, &mut out).release(Mode::Preview)?),
        Command::Publish => {
            let (mut api, repository) = github_from_env()?;
            Ok(runner(&mut git, &mut out).publish(&mut api, &repository)?)
        }
        Command::Commit => {
            let (mut api, repository) = github_from_env()?;
            let mut gates = GitHubGates {
                api: &mut api,
                repository: &repository,
                clock: SystemClock,
            };
            Ok(runner(&mut git, &mut out).release(Mode::Commit(&mut gates))?)
        }
    }
}

fn runner<'a>(git: &'a mut ProcessGit, out: &'a mut dyn Write) -> Runner<'a> {
    Runner {
        git,
        today: Timestamp::now(),
        out,
    }
}

fn github_from_env() -> Result<(GitHub, Repository), github::EnvError> {
    github::from_env(|key| std::env::var(key).ok(), Box::new(HttpTransport))
}
