//! The first line of a journal a Rust build wrote, told apart from another build's journal before any event is
//! decoded. Every reader classifies the first line here: [`crate::recorded_build`] and the event reader alike.

use serde::Deserialize;
use togi_machine::serde_object::{self, Object};

use crate::build::Build;

#[derive(Deserialize)]
struct Header {
    kind: String,
    #[serde(deserialize_with = "serde_object::deserialize")]
    data: HeaderData,
}

#[derive(Deserialize)]
struct HeaderData {
    #[serde(rename = "session")]
    _session: String,
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

/// The build stamp of `line` when it is a Rust journal header: a `session.start` object whose `data` holds a
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

/// Whether an unterminated first line is another build's journal rather than a first write that was cut short. A
/// line whose JSON simply ends early may be the start of a Rust header a crash tore, which the journal repairs by
/// truncating it; a line that is complete JSON or cannot be the start of JSON, and is not a header, is not, and
/// nothing may repair it.
pub(crate) fn foreign_unterminated(line: &[u8]) -> bool {
    let cut_short = matches!(
        serde_json::from_slice::<serde::de::IgnoredAny>(line),
        Err(error) if error.is_eof()
    );
    !cut_short && classify(line).is_none()
}

#[cfg(test)]
mod tests;
