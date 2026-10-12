//! `togi completion <shell>`: the completion script for one shell, generated from the clap
//! definition so that every command added later is completed without a second list.

use std::sync::LazyLock;

use clap::{Args as ClapArgs, ValueEnum};
use clap_complete::Generator;
use clap_complete_nushell::Nushell;

use crate::cli::{Context, command, examples};
use crate::exit::Exit;

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[
        (
            "source <(togi completion bash)",
            "Load completions in bash, such as from ~/.bashrc",
        ),
        (
            "source <(togi completion zsh)",
            "Load completions in zsh after compinit",
        ),
        ("togi completion fish | source", "Load completions in fish"),
        (
            "togi completion nushell | save -f togi.nu",
            "Write a script for nushell's source command",
        ),
    ])
});

/// The shells with a script.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Nushell,
}

/// Print a shell completion script
///
/// Print the completion script for one shell to stdout. It completes command names, each command's flags and their values where a value has a known
/// set or kind, such as paths for `--state-dir`. The Nix package installs all four scripts;
/// regenerate a script you saved yourself after updating togi.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    /// The shell to print the script for
    #[arg(value_enum)]
    pub shell: Shell,
}

/// The completion script of `shell` for the whole command line.
#[must_use]
pub fn script(shell: Shell) -> Vec<u8> {
    match shell {
        Shell::Bash => generate(clap_complete::Shell::Bash),
        Shell::Zsh => generate(clap_complete::Shell::Zsh),
        Shell::Fish => generate(clap_complete::Shell::Fish),
        Shell::Nushell => generate(Nushell),
    }
}

fn generate(generator: impl Generator) -> Vec<u8> {
    let mut command = command();
    let name = command.get_name().to_owned();
    let mut script = Vec::new();
    clap_complete::generate(generator, &mut command, name, &mut script);
    script
}

/// Writes the script to stdout in one piece, so a failed write is one error and never a script
/// cut short with status 0.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    let script = script(args.shell);
    match ctx.out.write_all(&script).and_then(|()| ctx.out.flush()) {
        Ok(()) => Exit::Ok,
        Err(error) => ctx.error(format_args!("completion: write the script: {error}")),
    }
}

#[cfg(test)]
mod tests;
