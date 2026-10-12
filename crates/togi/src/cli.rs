//! The top-level parser, the dispatch and the mapping of parse outcomes to streams and statuses.

use std::ffi::OsString;
use std::fmt::{Display, Write as _};
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{CommandFactory, Parser, Subcommand};

use crate::args::StateDirArgs;
use crate::exit::Exit;
use crate::{completion, events, status};

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
    Events(events::Args),
    Status(status::Args),
}

impl Command {
    /// Runs the command and returns the exit status.
    pub fn run(self, ctx: &mut Context<'_>) -> Exit {
        match self {
            Self::Completion(args) => completion::run(&args, ctx),
            Self::Events(args) => events::run(&args, ctx),
            Self::Status(args) => status::run(&args, ctx),
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
    let args = config_after_command(args.into_iter().map(Into::into).collect());
    match Cli::try_parse_from(&args) {
        Ok(cli) => {
            let mut ctx = Context {
                state_dir: &cli.state_dir.state_dir,
                out,
                err,
            };
            cli.command.run(&mut ctx)
        }
        Err(error) => report(&config_scope_error(error, &args), out, err),
    }
}

/// The commands that take `--config`, as the usage error names them.
const CONFIG_SCOPE: &str = "--config applies only to doctor, run and reset";

/// The flags written before the command name, as [`split_leading`] finds them.
struct Leading {
    /// The `--state-dir` flags with their values.
    state_dir: Vec<OsString>,
    /// The `--config` flags with their values.
    config: Vec<OsString>,
    /// The index of the command name, or the length when no command follows.
    command: usize,
}

/// Reads the leading flags. Only `--config` and `--state-dir` may precede the command name, each
/// with a value; `None` when an argument of any other shape comes first. The value of either flag
/// is never taken for the command name.
fn split_leading(args: &[OsString]) -> Option<Leading> {
    let mut leading = Leading {
        state_dir: Vec::new(),
        config: Vec::new(),
        command: 1,
    };
    while let Some(token) = args.get(leading.command).and_then(|token| token.to_str()) {
        let index = leading.command;
        let takes_value = |name: &str| token == name && index + 1 < args.len();
        if takes_value("--config") {
            leading.config.extend_from_slice(&args[index..index + 2]);
            leading.command += 2;
        } else if token.starts_with("--config=") {
            leading.config.push(args[index].clone());
            leading.command += 1;
        } else if takes_value("--state-dir") {
            leading.state_dir.extend_from_slice(&args[index..index + 2]);
            leading.command += 2;
        } else if token.starts_with("--state-dir=") {
            leading.state_dir.push(args[index].clone());
            leading.command += 1;
        } else if token.starts_with('-') {
            return None;
        } else {
            break;
        }
    }
    Some(leading)
}

/// Moves a `--config <path>` or `--config=<path>` written before the command name to just after
/// it, so that it is the command's own flag wherever it was written. Only the flags `--config`
/// and `--state-dir` may precede the command name; arguments of any other shape are left as
/// they are, and so are arguments without a command name.
#[must_use]
pub fn config_after_command(args: Vec<OsString>) -> Vec<OsString> {
    let Some(leading) = split_leading(&args) else {
        return args;
    };
    if leading.config.is_empty() || leading.command >= args.len() {
        return args;
    }
    let mut normalized = vec![args[0].clone()];
    normalized.extend(leading.state_dir);
    normalized.push(args[leading.command].clone());
    normalized.extend(leading.config);
    normalized.extend_from_slice(&args[leading.command + 1..]);
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
    let name = split_leading(args)
        .and_then(|leading| args.get(leading.command))
        .and_then(|arg| arg.to_str())
        .filter(|name| command.find_subcommand(name).is_some());
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
    match error.kind() {
        ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let _ = err.write_all(BANNER.as_bytes());
        }
        // Flags without a command, such as `--state-dir /x`: the same wordmark and help as no
        // arguments at all, in place of clap's error text.
        ErrorKind::MissingSubcommand => {
            let _ = err.write_all(BANNER.as_bytes());
            let _ = err.write_all(command().render_help().to_string().as_bytes());
            return Exit::Usage;
        }
        _ => {}
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
