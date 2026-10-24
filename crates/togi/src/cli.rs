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
use crate::system::{Real, System};
use crate::{
    completion, doctor, emergency_stop, events, night, night_end, night_guard, reset,
    restart_limit, status, watch,
};

const BANNER: &str = "   __              _\n  / /_____  ____ _(_)\n / __/ __ \\/ __ `/ /\n/ /_/ /_/ / /_/ / /\n\\__/\\____/\\__, /_/\n         /____/\n\n  per-core Curve Optimizer\n\n";

static VERSION: LazyLock<String> = LazyLock::new(togi_version::string);

/// The example `togi --help` and `togi run --help` both show for stopping after the confirmation cycle.
pub const CYCLES_EXAMPLE: (&str, &str) = (
    "sudo togi run --cycles 1",
    "Stop once phase 2's confirmation cycle has passed",
);

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[
        CYCLES_EXAMPLE,
        ("togi completion zsh", "Print the completion script for zsh"),
    ])
});

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
    Day(crate::day::Args),
    Completion(completion::Args),
    Doctor(doctor::Args),
    Events(events::Args),
    Night(night::Args),
    NightEnd(night_end::Args),
    NightGuard(night_guard::Args),
    Reset(reset::Args),
    RestartLimit(restart_limit::Args),
    EmergencyStop(emergency_stop::Args),
    Status(status::Args),
    Watch(watch::Args),
}

impl Command {
    /// Runs the command and returns the exit status.
    pub fn run(self, ctx: &mut Context<'_>) -> Exit {
        match self {
            Self::Run(args) => crate::run::run(&args, ctx),
            Self::Day(args) => crate::day::run(&args, ctx),
            Self::Completion(args) => completion::run(&args, ctx),
            Self::Doctor(args) => doctor::run(&args, ctx),
            Self::Events(args) => events::run(&args, ctx),
            Self::Night(args) => night::run(&args, ctx),
            Self::NightEnd(args) => night_end::run(&args, ctx),
            Self::NightGuard(args) => night_guard::run(&args, ctx),
            Self::Reset(args) => reset::run(&args, ctx),
            Self::RestartLimit(args) => restart_limit::run(&args, ctx),
            Self::EmergencyStop(args) => emergency_stop::run(&args, ctx),
            Self::Status(args) => status::run(&args, ctx),
            Self::Watch(args) => watch::run(&args, ctx),
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
    /// The host the command runs on.
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

/// [`run()`] on `system` instead of the host: commands can use memory files and locks or answer
/// hardware checks without touching the host. See [`crate::system`].
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

/// An argument before the command name, as [`token`] reads it.
enum Token {
    /// A `--config` or `--state-dir` flag spanning `width` arguments with its value.
    Flag { config: bool, width: usize },
    /// The command name, or the end of the arguments.
    Command,
    /// An argument of any other shape.
    Other,
}

/// Reads the argument at `index` before the command name. Only `--config` and `--state-dir` may
/// precede the command name, each with a value; the value of either flag is never taken for the
/// command name.
fn token(args: &[OsString], index: usize) -> Token {
    let Some(token) = args.get(index).and_then(|token| token.to_str()) else {
        return Token::Command;
    };
    let takes_value = |name: &str| token == name && index + 1 < args.len();
    if takes_value("--config") {
        Token::Flag {
            config: true,
            width: 2,
        }
    } else if token.starts_with("--config=") {
        Token::Flag {
            config: true,
            width: 1,
        }
    } else if takes_value("--state-dir") {
        Token::Flag {
            config: false,
            width: 2,
        }
    } else if token.starts_with("--state-dir=") {
        Token::Flag {
            config: false,
            width: 1,
        }
    } else if token.starts_with('-') {
        Token::Other
    } else {
        Token::Command
    }
}

/// The flags written before the command name, as [`split_leading`] finds them.
struct Leading {
    /// Whether a `--config` flag is among them.
    config: bool,
    /// The index of the command name, or the length when no command follows.
    command: usize,
}

/// Reads the leading flags; `None` when an argument of a shape other than [`Token::Flag`] comes
/// before the command name.
fn split_leading(args: &[OsString]) -> Option<Leading> {
    let mut leading = Leading {
        config: false,
        command: 1,
    };
    loop {
        match token(args, leading.command) {
            Token::Flag { config, width } => {
                leading.config |= config;
                leading.command += width;
            }
            Token::Command => return Some(leading),
            Token::Other => return None,
        }
    }
}

/// Moves a `--config <path>` or `--config=<path>` written before the command name to just after
/// it, so that it is the command's own flag wherever it was written. Only the flags `--config`
/// and `--state-dir` may precede the command name; arguments of any other shape are left as
/// they are, and so are arguments without a command name.
#[must_use]
pub fn config_after_command(mut args: Vec<OsString>) -> Vec<OsString> {
    let Some(Leading {
        config: true,
        mut command,
    }) = split_leading(&args)
    else {
        return args;
    };
    if command >= args.len() {
        return args;
    }
    // Each `--config` flag rotates to the end of the leading run, behind the command name and the
    // flags moved before it, so both the flags and the rest keep their order.
    let end = command + 1;
    let mut index = 1;
    while index < command {
        let Token::Flag { config, width } = token(&args, index) else {
            break;
        };
        if config {
            args[index..end].rotate_left(width);
            command -= width;
        } else {
            index += width;
        }
    }
    args
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
            Err(error) => {
                let _ = writeln!(
                    err,
                    "togi: write output: {}",
                    togi_render::error_chain(&error)
                );
                Exit::Error
            }
        };
    }
    let kind = error.kind();
    if kind == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        let _ = err.write_all(BANNER.as_bytes());
    } else if kind == ErrorKind::MissingSubcommand {
        // Flags without a command, such as `--state-dir /x`: the same wordmark and help as no
        // arguments at all, in place of clap's error text.
        let _ = err.write_all(BANNER.as_bytes());
        let _ = err.write_all(command().render_help().to_string().as_bytes());
        return Exit::Usage;
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
