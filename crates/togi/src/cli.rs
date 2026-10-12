//! The top-level parser, the dispatch and the mapping of parse outcomes to streams and statuses.

use std::ffi::OsString;
use std::fmt::{Display, Write as _};
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{CommandFactory, Parser, Subcommand};

use crate::args::StateDirArgs;
use crate::completion;
use crate::exit::Exit;
use crate::system::{Real, System};

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
    Run(crate::run::Args),
    Completion(completion::Args),
}

impl Command {
    /// Runs the command and returns the exit status.
    pub fn run(self, ctx: &mut Context<'_>) -> Exit {
        match self {
            Self::Run(args) => crate::run::run(&args, ctx),
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
    /// The host: platform, locks, machine and terminal.
    pub system: &'a dyn System,
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
    run_with(args, out, err, &Real)
}

/// [`run()`] against the given host instead of the real one.
pub fn run_with<I, T>(
    args: I,
    out: &mut dyn Write,
    err: &mut dyn Write,
    system: &dyn System,
) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = config_after_command(args.into_iter().map(Into::into).collect());
    match Cli::try_parse_from(&args) {
        Ok(cli) => {
            let mut ctx = Context {
                state_dir: &cli.state_dir.state_dir,
                out,
                err,
                system,
            };
            cli.command.run(&mut ctx)
        }
        Err(error) => report(&config_scope_error(error, &args), out, err),
    }
}

/// The commands that take `--config`, as the usage error names them.
const CONFIG_SCOPE: &str = "--config applies only to doctor, run and reset";

/// Moves a `--config <path>` or `--config=<path>` written before the command name to just after
/// it, so that it is the command's own flag wherever it was written. Only the flags `--config`
/// and `--state-dir` may precede the command name; arguments of any other shape are left as
/// they are, and so are arguments without a command name.
#[must_use]
pub fn config_after_command(args: Vec<OsString>) -> Vec<OsString> {
    let mut leading = Vec::new();
    let mut moved = Vec::new();
    let mut index = 1;
    while let Some(token) = args.get(index).and_then(|token| token.to_str()) {
        let takes_value = |name: &str| token == name && index + 1 < args.len();
        if takes_value("--config") {
            moved.extend_from_slice(&args[index..index + 2]);
            index += 2;
        } else if token.starts_with("--config=") {
            moved.push(args[index].clone());
            index += 1;
        } else if takes_value("--state-dir") {
            leading.extend_from_slice(&args[index..index + 2]);
            index += 2;
        } else if token.starts_with("--state-dir=") {
            leading.push(args[index].clone());
            index += 1;
        } else if token.starts_with('-') {
            return args;
        } else {
            break;
        }
    }
    if moved.is_empty() || index >= args.len() {
        return args;
    }
    let mut normalized = vec![args[0].clone()];
    normalized.extend(leading);
    normalized.push(args[index].clone());
    normalized.extend(moved);
    normalized.extend_from_slice(&args[index + 1..]);
    normalized
}

/// Words the unknown-flag error for `--config` as the scope rule: the flag belongs to three
/// commands and every other command, and the top level, reject it.
fn config_scope_error(error: clap::Error, args: &[OsString]) -> clap::Error {
    let rejected = error.kind() == ErrorKind::UnknownArgument
        && matches!(
            error.get(ContextKind::InvalidArg),
            Some(ContextValue::String(flag)) if flag == "--config" || flag.starts_with("--config=")
        );
    if !rejected {
        return error;
    }
    let mut command = command();
    command.build();
    let name = args
        .iter()
        .skip(1)
        .filter_map(|arg| arg.to_str())
        .find(|arg| command.find_subcommand(arg).is_some());
    match name.and_then(|name| command.find_subcommand_mut(name)) {
        Some(subcommand) => subcommand.error(ErrorKind::UnknownArgument, CONFIG_SCOPE),
        None => command.error(ErrorKind::UnknownArgument, CONFIG_SCOPE),
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
