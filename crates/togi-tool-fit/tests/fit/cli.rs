use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use flate2::read::MultiGzDecoder;
use sha2::{Digest as _, Sha256};
use togi_machine::Regime;

use crate::support::{fact, write_extract};

#[expect(
    clippy::disallowed_methods,
    reason = "real_process tests run the fitted-machine command"
)]
fn fit(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_togi-tool-fit"))
        .args(args)
        .output()
        .unwrap()
}

fn two_sessions(dir: &Path) -> std::path::PathBuf {
    let mut offsets = [0; 16];
    offsets[0] = -10;
    let facts = vec![
        fact("20260101T000000Z", 1, &offsets, Regime::R1, &[0], 60, false),
        fact("20260102T000000Z", 1, &offsets, Regime::R1, &[0], 60, true),
    ];
    write_extract(dir, &facts)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn real_process_forward_only_prints_the_check_and_writes_no_machine_files() {
    let dir = tempfile::tempdir().unwrap();
    let extract = two_sessions(dir.path());
    let ran = fit(&["--facts", extract.to_str().unwrap(), "--forward-only"]);
    let stdout = text(&ran.stdout);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert!(
        stdout.starts_with("Facts: sha256 "),
        "the report does not open with the rows' hash: {stdout}"
    );
    assert!(
        stdout.contains(", 2 rows, 2 sessions: not the original research artifact\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("20260102T000000Z ruleset=1 training_sessions=1: trials=1 failures=1"),
        "{stdout}"
    );
    assert!(stdout.contains("Pooled: "), "{stdout}");
    assert!(stdout.contains("Forward-chained elapsed: "), "{stdout}");
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries, [extract], "a forward-only run wrote a file");
}

#[test]
fn real_process_records_pair_with_a_rerun_and_compare_equal() {
    let dir = tempfile::tempdir().unwrap();
    let extract = two_sessions(dir.path());
    let facts = extract.to_str().unwrap();
    let runs = ["candidate.jsonl", "reference.jsonl"].map(|name| dir.path().join(name));
    for records in &runs {
        let ran = fit(&[
            "--facts",
            facts,
            "--forward-only",
            "--records",
            records.to_str().unwrap(),
        ]);
        assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    }
    let lines = std::fs::read_to_string(&runs[0]).unwrap();
    let kinds: Vec<&str> = lines
        .lines()
        .map(|line| line.split('"').nth(3).unwrap())
        .collect();
    assert_eq!(kinds, ["input", "trial", "session", "pooled"], "{lines}");
    assert_eq!(lines, std::fs::read_to_string(&runs[1]).unwrap());

    let metrics = dir.path().join("metrics.json");
    let [candidate, reference] = runs.each_ref().map(|path| path.to_str().unwrap());
    let ran = fit(&[
        "--compare",
        candidate,
        reference,
        "--metrics",
        metrics.to_str().unwrap(),
    ]);
    let stdout = text(&ran.stdout);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert!(
        stdout.contains("Pooled all: trials=1 failures=1 ") && stdout.contains(" ratio=1 "),
        "{stdout}"
    );
    assert!(
        stdout.ends_with("F3: candidate loss lower in 0 of 1 held-out sessions: fails\n"),
        "{stdout}"
    );
    let metrics: std::collections::BTreeMap<String, f64> =
        serde_json::from_str(&std::fs::read_to_string(&metrics).unwrap()).unwrap();
    assert_eq!(metrics["pooled.all.loss_ratio"], 1.0);
    assert_eq!(metrics["pooled.f3_sessions"], 1.0);

    let again = fit(&["--facts", facts, "--forward-only", "--records", candidate]);
    assert_eq!(
        again.status.code(),
        Some(1),
        "an existing records file was replaced"
    );

    let other = tempfile::tempdir().unwrap();
    let mut offsets = [0; 16];
    offsets[0] = -10;
    let changed = write_extract(
        other.path(),
        &[
            fact("20260101T000000Z", 1, &offsets, Regime::R1, &[0], 60, true),
            fact("20260102T000000Z", 1, &offsets, Regime::R1, &[0], 60, true),
        ],
    );
    let elsewhere = other.path().join("records.jsonl");
    let ran = fit(&[
        "--facts",
        changed.to_str().unwrap(),
        "--forward-only",
        "--records",
        elsewhere.to_str().unwrap(),
    ]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    let unpaired = fit(&["--compare", candidate, elsewhere.to_str().unwrap()]);
    assert_eq!(unpaired.status.code(), Some(1));
    assert!(
        text(&unpaired.stderr).contains("the records do not pair: the candidate read sha256 "),
        "{}",
        text(&unpaired.stderr)
    );
    assert!(unpaired.stdout.is_empty());
}

/// Two sessions recorded after the original research artifact: #480's spent score sessions.
fn later_sessions(dir: &Path) -> PathBuf {
    let mut offsets = [0; 16];
    offsets[0] = -10;
    let facts = vec![
        fact("20261004T211726Z", 1, &offsets, Regime::R1, &[0], 60, false),
        fact("20261010T230611Z", 1, &offsets, Regime::R1, &[0], 60, true),
    ];
    write_extract(dir, &facts)
}

#[test]
fn real_process_sessions_after_the_artifact_are_read_only_with_the_current_extract() {
    let dir = tempfile::tempdir().unwrap();
    let extract = later_sessions(dir.path());
    let out = dir.path().join("machines");
    let facts = extract.to_str().unwrap();
    for args in [
        &["--forward-only", "--seal", "0"][..],
        &["--out", out.to_str().unwrap()],
        &["--out", out.to_str().unwrap(), "--shared-voltage-in-sample"],
    ] {
        let mut all = vec!["--facts", facts];
        all.extend_from_slice(args);
        let ran = fit(&all);
        assert_eq!(ran.status.code(), Some(1), "{args:?}");
        assert_eq!(
            text(&ran.stderr),
            format!(
                "togi-tool-fit: {facts} holds session 20261004T211726Z, recorded after the original research artifact: only --current-extract reads later sessions, and no research campaign does\n"
            ),
            "{args:?}"
        );
        assert!(ran.stdout.is_empty(), "{args:?}");
        assert!(!out.exists(), "{args:?} created the output directory");
    }
    let ran = fit(&["--facts", facts, "--forward-only", "--current-extract"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert!(
        text(&ran.stdout).contains("20261010T230611Z ruleset=1 training_sessions=1: "),
        "{}",
        text(&ran.stdout)
    );
}

const COMMITTED: &[u8] = include_bytes!("../../../../tools/bench/facts/target.jsonl.gz");

#[test]
fn real_process_the_artifact_is_written_once_and_only_with_its_exact_rows() {
    let dir = tempfile::tempdir().unwrap();
    let committed = dir.path().join("committed.jsonl.gz");
    std::fs::write(&committed, COMMITTED).unwrap();
    let artifact = dir.path().join("artifact.jsonl.gz");
    let write = |facts: &Path, out: &Path| {
        fit(&[
            "--facts",
            facts.to_str().unwrap(),
            "--write-artifact",
            out.to_str().unwrap(),
        ])
    };
    let ran = write(&committed, &artifact);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert!(
        text(&ran.stdout).starts_with(&format!(
            "Wrote the original research artifact to {}: 3297 rows of 7 sessions, sha256 4b47590e12bd55bc73ee3ae468a782173acbafd4599892ef003e2371a8c066b7\n",
            artifact.display()
        )),
        "{}",
        text(&ran.stdout)
    );
    let written = std::fs::read(&artifact).unwrap();
    let mut rows = Vec::new();
    MultiGzDecoder::new(written.as_slice())
        .read_to_end(&mut rows)
        .unwrap();
    assert_eq!(
        Sha256::digest(&rows)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                write!(hex, "{byte:02x}").unwrap();
                hex
            }),
        "4b47590e12bd55bc73ee3ae468a782173acbafd4599892ef003e2371a8c066b7"
    );

    let again = write(&committed, &artifact);
    assert_eq!(again.status.code(), Some(1));
    assert!(
        text(&again.stderr).starts_with(&format!(
            "togi-tool-fit: create the artifact {}: ",
            artifact.display()
        )),
        "{}",
        text(&again.stderr)
    );
    assert_eq!(std::fs::read(&artifact).unwrap(), written);

    let elsewhere = dir.path().join("other.jsonl.gz");
    let synthetic = write(&two_sessions(dir.path()), &elsewhere);
    assert_eq!(synthetic.status.code(), Some(1));
    assert!(
        text(&synthetic.stderr).contains("take them from the extract committed at 455704be"),
        "{}",
        text(&synthetic.stderr)
    );
    assert!(!elsewhere.exists());
}

#[test]
fn real_process_sealing_every_held_out_session_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let extract = two_sessions(dir.path());
    let ran = fit(&[
        "--facts",
        extract.to_str().unwrap(),
        "--forward-only",
        "--seal",
        "1",
    ]);
    assert_eq!(ran.status.code(), Some(1));
    assert!(
        text(&ran.stderr).contains("--seal 1 leaves no held-out session to score (1 held out)"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn real_process_an_unreadable_extract_exits_1_with_the_error_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.gz");
    let out = dir.path().join("out");
    let out = out.to_str().unwrap();
    for extra in [
        &["--out", out][..],
        &["--forward-only"],
        &["--out", out, "--shared-voltage-in-sample"],
        &["--write-artifact", out],
    ] {
        let mut args = vec!["--facts", missing.to_str().unwrap()];
        args.extend_from_slice(extra);
        let ran = fit(&args);
        assert_eq!(ran.status.code(), Some(1), "{extra:?}");
        assert_eq!(
            text(&ran.stderr),
            format!(
                "togi-tool-fit: open extract {}: No such file or directory (os error 2)\n",
                missing.display()
            ),
            "{extra:?}"
        );
        assert!(ran.stdout.is_empty(), "{extra:?}");
    }
}

#[test]
fn real_process_flags_that_do_not_go_together_are_usage_errors() {
    for args in [
        &["--forward-only"][..],
        &["--facts", "f.gz"],
        &["--facts", "f.gz", "--forward-only", "--out", "o"],
        &[
            "--facts",
            "f.gz",
            "--forward-only",
            "--reference-machine",
            "m.json",
        ],
        &[
            "--facts",
            "f.gz",
            "--write-artifact",
            "a.gz",
            "--current-extract",
        ],
        &["--facts", "f.gz", "--write-artifact", "a.gz", "--out", "o"],
        &[
            "--facts",
            "f.gz",
            "--write-artifact",
            "a.gz",
            "--forward-only",
        ],
        &[
            "--facts",
            "f.gz",
            "--shared-voltage-in-sample",
            "--forward-only",
        ],
        &[
            "--facts",
            "f.gz",
            "--shared-voltage-in-sample",
            "--bootstrap",
            "0",
        ],
        &[
            "--facts",
            "f.gz",
            "--shared-voltage-in-sample",
            "--seed",
            "263",
        ],
        &[
            "--facts",
            "f.gz",
            "--shared-voltage-in-sample",
            "--seal",
            "0",
        ],
        &["--facts", "f.gz", "--seal", "1"],
        &["--facts", "f.gz", "--forward-only", "--seal", "-1"],
        &["--facts", "f.gz", "--forward-only", "--jobs", "0"],
        &["--facts", "f.gz", "--out", "o", "--bootstrap", "-1"],
        &["--facts", "f.gz", "--forward-only", "extra"],
        &["--facts", "f.gz", "--records", "r.jsonl"],
        &["--compare", "a.jsonl"],
        &["--compare", "a.jsonl", "b.jsonl", "--facts", "f.gz"],
        &["--compare", "a.jsonl", "b.jsonl", "--forward-only"],
        &["--compare", "a.jsonl", "b.jsonl", "--jobs", "1"],
        &["--metrics", "m.json"],
    ] {
        let ran = fit(args);
        assert_eq!(
            ran.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&ran.stderr)
        );
        assert!(ran.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn real_process_the_help_lists_the_modes() {
    let ran = fit(&["--help"]);
    assert_eq!(ran.status.code(), Some(0));
    let stdout = text(&ran.stdout);
    for flag in [
        "--forward-only",
        "--shared-voltage-in-sample",
        "--write-artifact",
        "--current-extract",
        "--records",
        "--compare",
        "--metrics",
        "--jobs",
        "--seal",
    ] {
        assert!(stdout.contains(flag), "{flag}: {stdout}");
    }
}

#[test]
fn real_process_an_ensemble_run_writes_the_machine_files_and_prints_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let extract = two_sessions(dir.path());
    let out = dir.path().join("machines");
    let ran = fit(&[
        "--facts",
        extract.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--bootstrap",
        "1",
        "--jobs",
        "2",
    ]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    for n in 0..2 {
        assert!(out.join(format!("target-fit-{n}.json")).is_file());
    }
    let stdout = text(&ran.stdout);
    assert!(stdout.contains("negative log likelihood"), "{stdout}");
    assert!(stdout.contains("Forward-chained check"), "{stdout}");
    assert!(stdout.contains("Fit elapsed: "), "{stdout}");
}

#[test]
fn real_process_the_shared_voltage_anchor_is_fitted_checked_and_written() {
    let dir = tempfile::tempdir().unwrap();
    let mut facts = Vec::new();
    for i in 0..149_u64 {
        let offset = if i >= 29 { -30 } else { 0 };
        let loaded: Vec<i64> = (0..16).collect();
        facts.push(fact(
            "20260101T000000Z",
            i + 1,
            &[offset; 16],
            Regime::R7,
            &loaded,
            600,
            i == 0,
        ));
    }
    let extract = write_extract(dir.path(), &facts);
    let out = dir.path().join("machines");
    let ran = fit(&[
        "--facts",
        extract.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--shared-voltage-in-sample",
    ]);
    let stdout = text(&ran.stdout);
    assert_eq!(ran.status.code(), Some(0), "{stdout}{}", text(&ran.stderr));
    assert!(out.join("target-shared-voltage.json").is_file());
    assert!(stdout.contains("Wrote IN-SAMPLE anchor: "), "{stdout}");
    assert!(stdout.contains("Fit elapsed: "), "{stdout}");
    assert!(!stdout.contains("Forward-chained"), "{stdout}");
    assert!(!out.join("target-fit-0.json").exists());
}
