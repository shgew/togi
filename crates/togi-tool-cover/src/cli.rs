//! The command line: `--lcov TRACE --build-dir DIR --base REV`, each also `-name` or `--name=value`.

use std::path::PathBuf;

use crate::Error;

pub const USAGE: &str = "\
Usage: cover --lcov TRACE --build-dir DIR --base REV

  --lcov TRACE      lcov trace written by cargo llvm-cov
  --build-dir DIR   profile directory of the coverage build, whose deps and build subdirectories hold its dep-info files
  --base REV        revision whose merge base with HEAD the working tree is compared against
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Report(Options),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub lcov: PathBuf,
    pub build_dir: PathBuf,
    pub base: String,
}

pub fn parse(args: &[String]) -> Result<Command, Error> {
    let (mut lcov, mut build_dir, mut base) = (None, None, None);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            if args.next().is_some() {
                return Err(Error::Usage);
            }
            break;
        }
        let Some(flag) = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')) else {
            return Err(Error::Usage);
        };
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (flag, None),
        };
        let slot = match name {
            "h" | "help" => return Ok(Command::Help),
            "lcov" => &mut lcov,
            "build-dir" => &mut build_dir,
            "base" => &mut base,
            _ => return Err(Error::UnknownFlag(arg.clone())),
        };
        let value = match inline {
            Some(value) => value,
            None => args
                .next()
                .cloned()
                .ok_or_else(|| Error::MissingValue(arg.clone()))?,
        };
        *slot = Some(value);
    }
    let nonempty = |value: Option<String>| value.filter(|value| !value.is_empty());
    match (nonempty(lcov), nonempty(build_dir), nonempty(base)) {
        (Some(lcov), Some(build_dir), Some(base)) => Ok(Command::Report(Options {
            lcov: lcov.into(),
            build_dir: build_dir.into(),
            base,
        })),
        _ => Err(Error::Usage),
    }
}

#[cfg(test)]
#[path = "tests/cli.rs"]
mod tests;
