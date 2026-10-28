//! `manifest.json`: everything a coordinator needs from the frozen pull request, and everything render
//! reads from it.

use serde::Serialize;
use togi_machine::object_only;

use crate::diff::FileDiff;
use crate::headlines::HeadLines;
use crate::text::{join, json_indented, nullable};
use crate::{Error, files};

pub const MANIFEST_VERSION: i64 = 2;

object_only! {
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    pub struct Manifest {
        #[serde(default, deserialize_with = "nullable")]
        pub version: i64,
        #[serde(default, deserialize_with = "nullable")]
        pub repository: String,
        #[serde(default, deserialize_with = "nullable")]
        pub pull_request: i64,
        #[serde(default, deserialize_with = "nullable")]
        pub head_sha: String,
        #[serde(default, deserialize_with = "nullable")]
        pub base_sha: String,
        /// The record this one follows; null in a first record.
        #[serde(default)]
        pub previous: Option<PreviousRecord>,
        /// Why a re-review covers the whole layer instead of a delta.
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "String::is_empty"
        )]
        pub fallback: String,
        /// True when every patch is unchanged: the record carries forward and the new head needs only a check.
        #[serde(default, deserialize_with = "nullable")]
        pub carry_forward: bool,
        /// The files of this record's diff: the pull request's, or the delta's on a re-review.
        #[serde(default, deserialize_with = "nullable")]
        pub files: Vec<ManifestFile>,
        /// Included changed lines as the frozen head numbers them, keyed by its paths.
        #[serde(default, deserialize_with = "nullable")]
        pub head_lines: HeadLines,
        /// F of the sizing rule, over the included files.
        #[serde(default, deserialize_with = "nullable")]
        pub included_files: i64,
        /// L of the sizing rule, over the included files.
        #[serde(default, deserialize_with = "nullable")]
        pub included_lines: i64,
        #[serde(default, deserialize_with = "nullable")]
        pub reviewers: i64,
        /// The small-pull-request rule applied to the pull request's whole diff.
        #[serde(default, deserialize_with = "nullable")]
        pub small: bool,
        #[serde(default, deserialize_with = "nullable")]
        pub pr_files: i64,
        #[serde(default, deserialize_with = "nullable")]
        pub pr_lines: i64,
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "Vec::is_empty"
        )]
        pub patches: Vec<PatchStatus>,
        /// Null until `just cover` output arrives.
        #[serde(default)]
        pub coverage: Option<Coverage>,
    }

    /// The record a re-review follows.
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    pub struct PreviousRecord {
        #[serde(default, deserialize_with = "nullable")]
        pub url: String,
        #[serde(default, deserialize_with = "nullable")]
        pub head_sha: String,
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "String::is_empty"
        )]
        pub base_sha: String,
        /// The previous record's verdict; empty for a version 1 or 2 record, which has none. Only a `success`
        /// record carries forward.
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "String::is_empty"
        )]
        pub verdict: String,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    pub struct ManifestFile {
        #[serde(default, deserialize_with = "nullable")]
        pub path: String,
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "String::is_empty"
        )]
        pub old_path: String,
        #[serde(default, deserialize_with = "nullable")]
        pub status: String,
        #[serde(default, deserialize_with = "nullable")]
        pub added: i64,
        #[serde(default, deserialize_with = "nullable")]
        pub removed: i64,
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "is_false"
        )]
        pub binary: bool,
        /// The reason the review skips the file; empty for an included file.
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "String::is_empty"
        )]
        pub exclusion: String,
        /// The included file's hunk file, relative to the snapshot directory.
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "String::is_empty"
        )]
        pub hunk_file: String,
    }

    /// One patch of a re-review's range-diff.
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    pub struct PatchStatus {
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "String::is_empty"
        )]
        pub old: String,
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "String::is_empty"
        )]
        pub new: String,
        #[serde(default, deserialize_with = "nullable")]
        pub subject: String,
        /// `unchanged`, `changed`, `added` or `removed`.
        #[serde(default, deserialize_with = "nullable")]
        pub status: String,
        /// An unchanged patch that range-diff showed as changed because only its context lines differ.
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "is_false"
        )]
        pub context_only: bool,
        /// A merge commit the previous head lacks, classified by its remerge diff, which range-diff never
        /// pairs.
        #[serde(
            default,
            deserialize_with = "nullable",
            skip_serializing_if = "is_false"
        )]
        pub merge: bool,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    pub struct Coverage {
        /// The ranges in the input, before dropping those outside the hunks.
        #[serde(default, deserialize_with = "nullable")]
        pub reported: i64,
        #[serde(default, deserialize_with = "nullable")]
        pub ranges: Vec<CoverRange>,
        #[serde(default, deserialize_with = "nullable")]
        pub not_built: Vec<NotBuilt>,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    pub struct CoverRange {
        #[serde(default, deserialize_with = "nullable")]
        pub path: String,
        #[serde(default, deserialize_with = "nullable")]
        pub first: i64,
        #[serde(default, deserialize_with = "nullable")]
        pub last: i64,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    pub struct NotBuilt {
        #[serde(default, deserialize_with = "nullable")]
        pub path: String,
        #[serde(default, deserialize_with = "nullable")]
        pub target: String,
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes the field by reference"
)]
fn is_false(value: &bool) -> bool {
    !value
}

impl Manifest {
    /// The files the review covers.
    pub fn included(&self) -> impl Iterator<Item = &ManifestFile> {
        self.files.iter().filter(|f| f.exclusion.is_empty())
    }

    #[expect(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "the coverage rule names the `.go` and `.rs` files exactly"
    )]
    pub fn changes_code(&self) -> bool {
        self.included()
            .any(|f| f.path.ends_with(".go") || f.path.ends_with(".rs"))
    }
}

/// The sizing rule of `.omp/commands/review-pr.md` applied to the included files: `l` lines added plus
/// removed over `f` files.
pub fn reviewer_count(l: i64, f: i64) -> i64 {
    if f == 0 {
        0
    } else if l < 100 || f <= 2 {
        1
    } else if l < 500 {
        f.min(2)
    } else if l < 5000 {
        ceil_div(f, 3).clamp(2, 4)
    } else {
        ceil_div(f, 2).min(8)
    }
}

fn ceil_div(a: i64, b: i64) -> i64 {
    (a + b - 1) / b
}

const SMALL_PATHS: [&str; 5] = [
    "crates/togi-smu",
    "crates/togi-trial",
    "crates/togi-session",
    "crates/togi-journal",
    "nix",
];

/// The small-pull-request rule applied to the whole diff: at most 2 files and under 100 changed lines,
/// every file counted, none under a protected path.
pub fn is_small(files: &[FileDiff]) -> bool {
    let protected = |p: &str| {
        SMALL_PATHS.iter().any(|dir| {
            p.strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/'))
        })
    };
    if files
        .iter()
        .any(|f| protected(&f.path) || protected(&f.old_path))
    {
        return false;
    }
    files.len() <= 2 && total_lines(files) < 100
}

pub fn total_lines(files: &[FileDiff]) -> i64 {
    files.iter().map(|f| f.added + f.removed).sum()
}

pub fn hunk_file_name(index: usize) -> String {
    format!("files/{index:03}.diff")
}

pub fn read_manifest(dir: &str) -> Result<Manifest, Error> {
    let bytes = files::read(&join(dir, "manifest.json"))
        .map_err(|error| Error::wrap("read manifest", error))?;
    let m: Manifest = serde_json::from_slice(&bytes)
        .map_err(|error| Error::wrap(format!("decode manifest in {dir}"), error))?;
    if m.version != MANIFEST_VERSION {
        return Err(Error::new(format!(
            "manifest in {dir} has version {}; this tool reads version {MANIFEST_VERSION}",
            m.version
        )));
    }
    Ok(m)
}

pub fn write_manifest(dir: &str, m: &Manifest) -> Result<(), Error> {
    let mut text = json_indented(m).map_err(|error| Error::wrap("encode manifest", error))?;
    text.push('\n');
    files::write(&join(dir, "manifest.json"), text.as_bytes())
        .map_err(|error| Error::wrap("write manifest", error))
}
