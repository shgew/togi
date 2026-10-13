//! The first line of a journal this build wrote, its header, told apart from another build's journal before any
//! event is decoded. Every reader classifies the first line here: [`crate::recorded_build`] and the event reader
//! alike.

use serde::Deserialize;
use togi_machine::serde_object::{self, Object};

use crate::build::Build;
use crate::session_id::SessionId;

#[derive(Deserialize)]
struct Header {
    kind: String,
    #[serde(deserialize_with = "serde_object::deserialize")]
    data: HeaderData,
}

#[derive(Deserialize)]
struct HeaderData {
    #[serde(rename = "session")]
    _session: SessionId,
    #[serde(deserialize_with = "serde_object::deserialize")]
    build: HeaderBuild,
}

/// The build stamp's fields, without refusing the ones a newer build may add.
#[derive(Deserialize)]
struct HeaderBuild {
    version: String,
    #[serde(default)]
    rev: Option<String>,
    ruleset: u32,
    schema: u32,
    #[serde(default)]
    fixes: u32,
    evidence: u32,
}

/// The build stamp of `line` when it is a journal header: a `session.start` object whose `data` holds a
/// session and a build stamp. Fields the header does not need are ignored, so a newer build's additions keep its
/// journal recognisable and leave the stamp's verdict, and the strict decode after it, to refuse it. `None` for
/// any other line: malformed JSON, another kind, or a header without `data`, `session` or `build`.
pub(crate) fn classify(line: &[u8]) -> Option<Build> {
    let Object(header) = serde_json::from_slice::<Object<Header>>(line).ok()?;
    if header.kind != "session.start" {
        return None;
    }
    let stamp = header.data.build;
    Some(Build {
        version: stamp.version,
        rev: stamp.rev,
        ruleset: stamp.ruleset,
        schema: stamp.schema,
        fixes: stamp.fixes,
        evidence: stamp.evidence,
    })
}

#[cfg(test)]
mod tests;
