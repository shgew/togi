//! `togi completion <shell>`: the completion script for one shell, generated from the clap
//! definition so that every command added later is completed without a second list.

use std::sync::LazyLock;

use clap::{Args as ClapArgs, ValueEnum};
use clap_complete::Generator;
use clap_complete_nushell::Nushell;

use togi_journal::KindSelector;

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
        Shell::Bash => comma_kinds(&generate(clap_complete::Shell::Bash), &bash_kinds),
        Shell::Zsh => comma_kinds(&generate(clap_complete::Shell::Zsh), &zsh_kinds),
        Shell::Fish => comma_kinds(&generate(clap_complete::Shell::Fish), &fish_kinds),
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

/// Rewrites the one whole-value `events --kind` list clap generates into a completer of the item
/// after the last comma, which clap's static generators cannot express. The tests pin that the
/// list was found, so a clap upgrade that changes its form fails them instead of losing the
/// completion.
fn comma_kinds(script: &[u8], rewrite: &dyn Fn(&str, &str) -> String) -> Vec<u8> {
    let kinds = KindSelector::names().join(" ");
    rewrite(&String::from_utf8_lossy(script), &kinds).into_bytes()
}

/// Bash splits words on `=` but not on `,`, so `cur` is the whole list typed so far.
fn bash_kinds(script: &str, kinds: &str) -> String {
    script.replacen(
        &format!(r#"COMPREPLY=($(compgen -W "{kinds}" -- "${{cur}}"))"#),
        &format!(
            r#"local prefix=""
                    if [[ ${{cur}} == *,* ]]; then
                        prefix="${{cur%,*}},"
                    fi
                    COMPREPLY=($(compgen -P "${{prefix}}" -W "{kinds}" -- "${{cur##*,}}"))"#
        ),
        1,
    )
}

fn zsh_kinds(script: &str, kinds: &str) -> String {
    script.replacen(
        &format!(":kinds:({kinds})"),
        &format!(":kinds:_sequence compadd - {kinds}"),
        1,
    )
}

/// Fish filters the candidates against the whole token, so each one repeats the items before the
/// last comma.
fn fish_kinds(script: &str, kinds: &str) -> String {
    let clap_list: Vec<String> = kinds
        .split(' ')
        .map(|kind| format!(r"{kind}\t''"))
        .collect();
    let rewritten = script.replacen(
        &format!(r#"-a "{}""#, clap_list.join("\n")),
        "-a '(__togi_kinds)'",
        1,
    );
    format!(
        r#"function __togi_kinds
    set -l token (commandline -ct)
    set token (string replace -r -- '^--?[^=]*=' '' "$token")
    set -l prefix (string replace -r -- '[^,]*$' '' "$token")
    for kind in {kinds}
        echo "$prefix$kind"
    end
end

{rewritten}"#
    )
}

/// Writes the script to stdout in one piece, so a failed write is one error and never a script
/// cut short with status 0.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    let script = script(args.shell);
    match ctx.out.write_all(&script).and_then(|()| ctx.out.flush()) {
        Ok(()) => Exit::Ok,
        Err(error) => ctx.error(format_args!(
            "completion: write the script: {}",
            togi_render::error_chain(&error)
        )),
    }
}

#[cfg(test)]
mod tests;
