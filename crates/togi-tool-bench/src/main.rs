use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;

use clap::{Parser, Subcommand};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use togi_tool_bench::Fleet;

mod same_cli;

const USAGE: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "togi-tool-bench",
    version = togi_version::version(),
    about = "Compare simulated journals on a fixed set of scripted sessions"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Compare exact journals for the fixed scripted mechanics set, or its smoke subset.
    Same(same_cli::SameArgs),
}

fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(cli) => match cli.command {
            Command::Same(args) => same_cli::run(args),
        },
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
    let _ = writeln!(io::stderr(), "togi-tool-bench: {error}");
    ExitCode::FAILURE
}

fn default_cache() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty());
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.map(|home| {
                let mut dir = PathBuf::from(home);
                dir.push(if cfg!(target_os = "macos") {
                    "Library/Caches"
                } else {
                    ".cache"
                });
                dir
            })
        })?;
    Some(base.join("togi").join("bench"))
}

/// Stops the fleet on SIGINT, SIGTERM and SIGHUP without adding those signals to the mask inherited by children.
fn cancel_on_signals(fleet: &Arc<Fleet>) -> io::Result<()> {
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    let fleet = Arc::clone(fleet);
    thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            for _ in signals.forever() {
                fleet.cancel();
            }
        })
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::{CommandFactory as _, Parser as _};
    use test_case::test_case;

    #[test_case("day-night"; "day night gate")]
    #[test_case("research"; "campaign dispatcher")]
    #[test_case("sanity"; "metric bench sanity")]
    fn retired_commands_are_refused(command: &str) {
        let error = Cli::try_parse_from(["togi-tool-bench", command]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn help_lists_only_retained_commands() {
        let mut command = Cli::command();
        let help = command.render_long_help().to_string();
        assert!(help.contains("same"));
        assert!(!help.contains("sanity"));
        assert!(!help.contains("--baseline"));
        assert!(!help.contains("day-night"));
        assert!(!help.contains("research"));
    }
}
