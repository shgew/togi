//! The report `just cover` prints and `just reviews snapshot --cover` reads.

use std::io::Write;
use std::path::Path;

use crate::cli::Options;
use crate::diff::{intersect, parse_diff};
use crate::host::Host;
use crate::lcov::{parse_lcov, unreached};
use crate::tree::Tree;
use crate::unbuilt::{built_sources, not_built};
use crate::{Error, process};

/// What one report reads.
#[derive(Clone, Copy, Debug)]
pub struct Inputs<'a> {
    /// `git diff -U0 --no-prefix` of the Rust files against the merge base.
    pub diff: &'a [u8],
    /// The LCOV trace of the coverage run.
    pub trace: &'a str,
    /// The coverage build's profile directory, holding `deps` and `build`.
    pub build_dir: &'a Path,
    /// The repository root, which the trace's and dep-info's absolute paths are relative to.
    pub root: &'a Path,
    pub base: &'a str,
    pub host: &'a Host,
}

/// Writes the changed Rust lines the trace leaves unreached, one `path:first-last` range per line,
/// then `path: not built for <triple>` for each changed file with code the build left out.
pub fn report(tree: &dyn Tree, inputs: Inputs<'_>, out: &mut dyn Write) -> Result<(), Error> {
    let Inputs {
        diff,
        trace,
        build_dir,
        root,
        base,
        host,
    } = inputs;
    let changed = parse_diff(diff)?;
    if changed.is_empty() {
        return writeln!(out, "No Rust lines changed since {base}.").map_err(Error::Write);
    }
    let measured = parse_lcov(trace, root)?;
    let built = built_sources(tree, build_dir, root)?;
    let unbuilt = not_built(tree, root, &changed, &built, &measured, &host.cfg)?;
    let gaps = intersect(&changed, &unreached(&measured));
    if gaps.is_empty() {
        writeln!(
            out,
            "Every changed Rust line the trace measures ran in a test since {base}."
        )
        .map_err(Error::Write)?;
    }
    for gap in gaps {
        writeln!(out, "{gap}").map_err(Error::Write)?;
    }
    for path in unbuilt {
        writeln!(out, "{path}: not built for {}", host.triple).map_err(Error::Write)?;
    }
    Ok(())
}

/// Writes the report for `options` of the repository at `root`.
pub fn run(options: &Options, root: &Path, out: &mut dyn Write) -> Result<(), Error> {
    let trace = std::fs::read(root.join(&options.lcov)).map_err(|source| Error::ReadTrace {
        path: options.lcov.clone(),
        source,
    })?;
    let host = Host::of_rustc(root)?;
    let diff = process::rust_diff(root, &options.base)?;
    report(
        &crate::tree::DiskTree,
        Inputs {
            diff: &diff,
            trace: &String::from_utf8_lossy(&trace),
            build_dir: &root.join(&options.build_dir),
            root,
            base: &options.base,
            host: &host,
        },
        out,
    )
}

#[cfg(test)]
#[path = "tests/report.rs"]
mod tests;
