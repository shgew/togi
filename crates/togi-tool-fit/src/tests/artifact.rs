use std::io::Read as _;
use std::path::{Path, PathBuf};

use flate2::read::MultiGzDecoder;
use togi_facts::Decisive;
use togi_machine::Regime;
use togi_tool_trialfacts::{TrialFact, encode};

use super::fact_at;
use crate::artifact::{CONFIRMATION, DEVELOPMENT, SHA256, Sessions, admit, select};
use crate::error::Error;
use crate::{Ensemble, Mode, report_extract};

const COMMITTED: &[u8] = include_bytes!("../../../../tools/bench/facts/target.jsonl.gz");

fn path() -> &'static Path {
    Path::new("facts.jsonl.gz")
}

fn decompress(gzip: &[u8]) -> Vec<u8> {
    let mut rows = Vec::new();
    MultiGzDecoder::new(gzip).read_to_end(&mut rows).unwrap();
    rows
}

fn rows(facts: &[TrialFact]) -> Vec<u8> {
    let mut gzip = Vec::new();
    encode(&mut gzip, facts).unwrap();
    decompress(&gzip)
}

fn trial(session: &str, seq: u64) -> TrialFact {
    let mut offsets = [0; 16];
    offsets[0] = -20;
    fact_at(
        session,
        seq,
        &offsets,
        Regime::R1,
        0,
        &[0],
        60,
        Decisive::Pass,
    )
}

fn artifact_rows() -> Vec<u8> {
    select(path(), &decompress(COMMITTED)).unwrap().0
}

#[test]
fn the_committed_extracts_artifact_sessions_are_the_original_artifact() {
    let (rows, count) = select(path(), &decompress(COMMITTED)).unwrap();
    assert_eq!(count, 3297);
    let extract = admit(path(), &rows, Sessions::Artifact).unwrap();
    assert!(extract.is_artifact());
    assert_eq!(extract.sha256, SHA256);
    assert_eq!((extract.rows, extract.sessions), (3297, 7));
    let newest = extract.facts.iter().map(|fact| fact.session).max().unwrap();
    assert_eq!(newest.to_string(), CONFIRMATION);
    assert_eq!(DEVELOPMENT.len() + 1, extract.sessions);
}

#[test]
fn the_committed_extract_is_refused_unless_the_run_names_the_current_extract() {
    let rows = decompress(COMMITTED);
    match admit(path(), &rows, Sessions::Artifact) {
        Err(Error::LaterSession { session, .. }) => assert_eq!(session, "20261004T211726Z"),
        other => panic!("{other:?}"),
    }
    let current = admit(path(), &rows, Sessions::Current).unwrap();
    assert!(!current.is_artifact());
    assert!(current.sessions > 7, "{}", current.sessions);
}

#[test]
fn an_edited_or_partial_artifact_is_refused() {
    let full = artifact_rows();
    let mut lines: Vec<&[u8]> = full.split_inclusive(|&byte| byte == b'\n').collect();
    lines.remove(0);
    let dropped: Vec<u8> = lines.concat();
    let development: Vec<u8> = full
        .split_inclusive(|&byte| byte == b'\n')
        .filter(|line| {
            !line
                .windows(CONFIRMATION.len())
                .any(|w| w == CONFIRMATION.as_bytes())
        })
        .flatten()
        .copied()
        .collect();
    for rows in [dropped, development] {
        match admit(path(), &rows, Sessions::Artifact) {
            Err(Error::NotTheArtifact { sha256, .. }) => assert_ne!(sha256, SHA256),
            other => panic!("{other:?}"),
        }
        assert!(admit(path(), &rows, Sessions::Current).is_ok());
    }
}

#[test]
fn a_session_after_the_confirmation_is_refused_even_with_a_suffix() {
    for later in ["20261004T000701Z-2", "20261004T000702Z", "20261010T230611Z"] {
        let facts = [trial("20260101T000000Z", 1), trial(later, 2)];
        match admit(path(), &rows(&facts), Sessions::Artifact) {
            Err(Error::LaterSession { session, .. }) => assert_eq!(session, later),
            other => panic!("{later}: {other:?}"),
        }
    }
}

#[test]
fn synthetic_sessions_are_admitted_and_are_not_the_artifact() {
    let facts = [trial("20260101T000000Z", 1), trial("20260102T000000Z", 2)];
    let extract = admit(path(), &rows(&facts), Sessions::Artifact).unwrap();
    assert!(!extract.is_artifact());
    assert_eq!((extract.rows, extract.sessions), (2, 2));
}

#[test]
fn materializing_from_an_extract_without_the_artifacts_rows_is_refused() {
    let facts = [trial(DEVELOPMENT[0], 1), trial(CONFIRMATION, 2)];
    match select(path(), &rows(&facts)) {
        Err(Error::ArtifactSource { sha256, .. }) => assert_ne!(sha256, SHA256),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_report_names_the_rows_and_whether_the_historical_confirmation_is_scored() {
    let extract = admit(path(), &artifact_rows(), Sessions::Artifact).unwrap();
    let report = |mode: Mode| {
        let mut out = Vec::new();
        report_extract(&mut out, &extract, &mode).unwrap();
        String::from_utf8(out).unwrap()
    };
    let facts =
        format!("Facts: sha256 {SHA256}, 3297 rows, 7 sessions: the original research artifact\n");
    let forward = |seal| Mode::Forward {
        seal,
        reference: Vec::new(),
    };
    assert_eq!(
        report(forward(1)),
        format!(
            "{facts}Historical confirmation {CONFIRMATION}: sealed, neither fitted nor scored\n"
        )
    );
    assert_eq!(
        report(forward(0)),
        format!(
            "{facts}Historical confirmation {CONFIRMATION}: scored, a historical check that is not blind\n"
        )
    );
    assert_eq!(
        report(Mode::Ensemble {
            ensemble: Ensemble {
                seed: 263,
                bootstrap: 0
            },
            out: PathBuf::from("out"),
        }),
        facts
    );
}
