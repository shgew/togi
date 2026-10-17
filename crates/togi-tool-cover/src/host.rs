//! The target the coverage build compiled for.

use std::path::Path;

use crate::Error;
use crate::cfg::HostCfg;
use crate::process::output;

/// The option `cargo llvm-cov` enables beyond the target's own: `cfg(coverage)`. `test` is not
/// one: unit-test builds set it, while the library builds that integration tests and dependents
/// link do not.
const COVERAGE_CFG: &str = "coverage";

/// The coverage build's target: its triple and the cfg options `rustc --print cfg` names for it.
#[derive(Clone, Debug)]
pub struct Host {
    pub triple: String,
    pub cfg: HostCfg,
}

impl Host {
    /// The host the `rustc` on the path compiles for, run in `dir`.
    pub fn of_rustc(dir: &Path) -> Result<Self, Error> {
        let version = output("rustc", &["-vV"], dir).map_err(|source| Error::Rustc {
            args: "-vV",
            source,
        })?;
        let printed = output("rustc", &["--print", "cfg"], dir).map_err(|source| Error::Rustc {
            args: "--print cfg",
            source,
        })?;
        Self::from_rustc(
            &String::from_utf8_lossy(&version),
            &String::from_utf8_lossy(&printed),
        )
    }

    /// The host from the output of `rustc -vV` and `rustc --print cfg`.
    pub fn from_rustc(version: &str, printed: &str) -> Result<Self, Error> {
        let triple = version
            .lines()
            .rev()
            .find_map(|line| line.trim().strip_prefix("host: "))
            .ok_or(Error::NoHost)?
            .to_owned();
        let options = std::iter::once(COVERAGE_CFG).chain(
            printed
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty()),
        );
        Ok(Self {
            triple,
            cfg: HostCfg::new(options),
        })
    }
}

#[cfg(test)]
#[path = "tests/host.rs"]
mod tests;
