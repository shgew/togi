//! The top-level parser, the dispatch and the mapping of parse outcomes to streams and statuses.

use std::ffi::OsString;
use std::fmt::{Display, Write as _};
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};

use crate::args::StateDirArgs;
use crate::completion;
use crate::exit::Exit;

const BANNER: &str = "   __              _\n  / /_____  ____ _(_)\n / __/ __ \\/ __ `/ /\n/ /_/ /_/ / /_/ / /\n\\__/\\____/\\__, /_/\n         /____/\n\n  per-core Curve Optimizer\n\n";

static VERSION: LazyLock<String> = LazyLock::new(togi_version::string);

static EXAMPLES: LazyLock<String> =
    LazyLock::new(|| examples(&[("togi completion zsh", "Print the completion script for zsh")]));

/// Finds and tests per-core Curve Optimizer offsets on Zen 5 desktop CPUs.
#[derive(Debug, Parser)]
#[command(
    name = "togi",
    version = VERSION.as_str(),
    after_help = EXAMPLES.as_str(),
    arg_required_else_help = true,
)]
pub struct Cli {
    #[command(flatten)]
    pub state_dir: StateDirArgs,

    #[command(subcommand)]
    pub command: Command,
}

/// The commands. One variant per command, holding the `Args` of its module; the first paragraph
/// of that struct's doc comment is the summary `togi --help` lists.
#[derive(Debug, Subcommand)]
pub enum Command {
    Completion(completion::Args),
}

impl Command {
    /// Runs the command and returns the exit status.
    pub fn run(self, ctx: &mut Context<'_>) -> Exit {
        match self {
            Self::Completion(args) => completion::run(&args, ctx),
        }
    }
}

/// What a command runs against: the shared flags and the streams it writes to.
pub struct Context<'a> {
    /// The `--state-dir` value.
    pub state_dir: &'a Path,
    /// Standard output.
    pub out: &'a mut dyn Write,
    /// Standard error.
    pub err: &'a mut dyn Write,
}

impl Context<'_> {
    /// Writes `togi: <message>` to stderr and returns [`Exit::Error`].
    pub fn error(&mut self, message: impl Display) -> Exit {
        let _ = writeln!(self.err, "togi: {message}");
        Exit::Error
    }
}

/// Parses `args` (the program name first), runs the command and returns the exit status.
///
/// Help and `--version` print to `out` and exit 0. A usage error prints to `err` and exits 2.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    match Cli::try_parse_from(args) {
        Ok(cli) => {
            let mut ctx = Context {
                state_dir: &cli.state_dir.state_dir,
                out,
                err,
            };
            cli.command.run(&mut ctx)
        }
        Err(error) => report(&error, out, err),
    }
}

fn report(error: &clap::Error, out: &mut dyn Write, err: &mut dyn Write) -> Exit {
    let text = error.render().to_string();
    if !error.use_stderr() {
        return match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
            Ok(()) => Exit::Ok,
            Err(e) => {
                let _ = writeln!(err, "togi: write output: {e}");
                Exit::Error
            }
        };
    }
    if error.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        let _ = err.write_all(BANNER.as_bytes());
    }
    let _ = err.write_all(text.as_bytes());
    Exit::Usage
}

/// The clap command definition, for completion and tests.
#[must_use]
pub fn command() -> clap::Command {
    Cli::command()
}

/// Renders an `Examples:` block, every description starting in one column.
#[must_use]
pub fn examples(rows: &[(&str, &str)]) -> String {
    let width = rows
        .iter()
        .map(|(command, _)| command.len())
        .max()
        .unwrap_or(0);
    let mut block = String::from("Examples:");
    for (command, text) in rows {
        let _ = write!(block, "\n  {command:<width$}   {text}");
    }
    block
}

#[cfg(test)]
mod tests;
