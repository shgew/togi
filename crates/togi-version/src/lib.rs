//! The togi version from `version.txt` and the revision a build stamps.

#[cfg(test)]
mod tests;

const VERSION: &str = include_str!("../../../version.txt");

const REV: &str = resolve_revision(option_env!("TOGI_REV"));

const fn resolve_revision(explicit: Option<&'static str>) -> &'static str {
    match explicit {
        Some(rev) if !rev.is_empty() => rev,
        _ => "dev",
    }
}

/// The release version read from version.txt, without surrounding whitespace.
#[must_use]
pub fn version() -> &'static str {
    VERSION.trim()
}

/// The revision the build was stamped with through `TOGI_REV`, or `dev` for a local cargo build.
#[must_use]
pub fn rev() -> &'static str {
    REV
}

/// The version and the revision, as `VERSION+REVISION`.
#[must_use]
pub fn string() -> String {
    format!("{}+{}", version(), rev())
}
