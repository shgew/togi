//! The build stamp this binary records in a journal and compares a journal's against.

use togi_journal::{Build, SCHEMA};

/// This binary's version, revision, schema, ruleset and evidence epoch.
#[must_use]
pub fn current() -> Build {
    Build {
        version: togi_version::version().to_owned(),
        rev: Some(togi_version::rev().to_owned()),
        ruleset: togi_tuner::RULESET,
        schema: SCHEMA,
        fixes: togi_defect::fixed_stamp(togi_defect::KNOWN_DEFECTS).0,
        evidence: togi_tuner::EVIDENCE_EPOCH,
    }
}
