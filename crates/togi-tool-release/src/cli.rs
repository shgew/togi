//! The command line, parsed as Go's `flag` package does: `-name` or `--name`, `-name=value`, a string flag's
//! value as the next argument, and parsing stops at the first non-flag argument or at `--`.

use std::path::PathBuf;

/// What to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Validate the fragments in the directory.
    Check(PathBuf),
    /// Print the release `origin/main` would make.
    Preview,
    /// Commit the release and check it out.
    Commit,
    /// Publish the release at `HEAD`.
    Publish,
}

/// The outcome of parsing the arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Parsed {
    Run(Command),
    /// `-h`, `-help` or `--help`: print [`USAGE`] and succeed.
    Help,
    /// A usage error: print the message, if any, and [`USAGE`], and exit 2.
    Usage(Option<String>),
}

/// The usage text, printed to standard error.
pub const USAGE: &str = "\
Usage: release [-commit | -publish | -check dir]

Without flags, prints the release the release workflow would make from origin/main.
  -check dir
    \tvalidate the changelog fragments in dir and exit (also part of the quality flake check)
  -commit
    \tcommit the next release on top of origin/main and check it out, refusing while any issue, open or closed, \
is labeled needs-hardware, once the check workflow passed on origin/main, waiting up to an hour for it (release \
workflow; reads GITHUB_API_URL, GITHUB_REPOSITORY and GH_TOKEN)
  -publish
    \tpublish the release version.txt names at HEAD, if not yet published (release workflow; reads \
GITHUB_API_URL, GITHUB_REPOSITORY and GH_TOKEN)
";

/// Parses the arguments after the program name.
pub fn parse(args: &[String]) -> Parsed {
    let mut check: Option<String> = None;
    let mut commit = false;
    let mut publish = false;
    let mut rest = args;
    while let Some((arg, tail)) = rest.split_first() {
        if arg.len() < 2 || !arg.starts_with('-') {
            break;
        }
        rest = tail;
        if arg == "--" {
            break;
        }
        let name = arg.strip_prefix("--").unwrap_or(&arg[1..]);
        if name.is_empty() || name.starts_with('-') || name.starts_with('=') {
            return usage(format!("bad flag syntax: {arg}"));
        }
        let (name, value) = match name.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (name, None),
        };
        match name {
            "check" => {
                let value = match value {
                    Some(value) => value.to_owned(),
                    None => match rest.split_first() {
                        Some((value, tail)) => {
                            rest = tail;
                            value.clone()
                        }
                        None => return usage(format!("flag needs an argument: -{name}")),
                    },
                };
                check = (!value.is_empty()).then_some(value);
            }
            "commit" | "publish" => {
                let set = match value.map(parse_bool) {
                    None => true,
                    Some(Some(set)) => set,
                    Some(None) => {
                        return usage(format!(
                            "invalid boolean value {:?} for -{name}: parse error",
                            value.unwrap_or_default()
                        ));
                    }
                };
                if name == "commit" {
                    commit = set;
                } else {
                    publish = set;
                }
            }
            "h" | "help" => return Parsed::Help,
            _ => return usage(format!("flag provided but not defined: -{name}")),
        }
    }
    if !rest.is_empty() {
        return Parsed::Usage(None);
    }
    match (check, commit, publish) {
        (Some(dir), false, false) => Parsed::Run(Command::Check(PathBuf::from(dir))),
        (None, false, false) => Parsed::Run(Command::Preview),
        (None, true, false) => Parsed::Run(Command::Commit),
        (None, false, true) => Parsed::Run(Command::Publish),
        _ => Parsed::Usage(None),
    }
}

fn usage(message: String) -> Parsed {
    Parsed::Usage(Some(message))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
#[path = "tests/cli.rs"]
mod tests;
