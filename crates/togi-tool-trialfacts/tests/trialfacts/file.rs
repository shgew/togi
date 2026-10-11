use std::collections::BTreeMap;
use std::io::{self, Write};

use flate2::Compression;
use flate2::write::GzEncoder;
use togi_facts::{Decisive, TrialClass};
use togi_machine::{CcdId, Condition, PerCore, Regime, Signal};
use togi_tool_trialfacts::{
    Class, Phase, ReadError, TrialFact, WriteError, decode, encode, read, write,
};

use crate::builders::{core, crash, fact, set, workload};

const FIRST: &str = "20260101T000000Z";

fn gz(text: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(text.as_bytes()).unwrap();
    encoder.finish().unwrap()
}

fn line(fact: &TrialFact) -> String {
    serde_json::to_string(fact).unwrap()
}

fn sample() -> Vec<TrialFact> {
    let mut telemetry = fact(
        FIRST,
        7,
        &[-30, -50],
        Decisive::Failure {
            signal: Signal::ComputationError,
            core: Some(core(1)),
            stalled: Some(core(0)),
        },
    );
    telemetry.class = Class::Trial(TrialClass {
        workload: workload(Regime::R7, 0),
        loaded: set(&[0, 1]),
        duration_s: 120,
    });
    telemetry.condition = Condition::Together;
    telemetry.phase = Some(Phase::Hunt);
    telemetry.duration_s = 0;
    telemetry.voltage_requests_v = [(core(0), 1.125), (core(1), 1.045_442_938_804_626_5)]
        .into_iter()
        .collect();
    telemetry.top_requesters = set(&[1]);
    telemetry.ccd_mhz = BTreeMap::from([(CcdId(0), 4800)]);
    let mut idle = fact(FIRST, 9, &[-10, -20], crash(None));
    idle.class = Class::Idle {
        loaded: set(&[0, 1]),
    };
    idle.condition = Condition::Parked;
    idle.phase = None;
    idle.trial = None;
    idle.duration_s = 0;
    vec![fact(FIRST, 4, &[-10, 0], Decisive::Pass), telemetry, idle]
}

struct Failing {
    accepted: usize,
}

impl Write for Failing {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.accepted == 0 {
            return Err(io::Error::other("destination rejected write"));
        }
        self.accepted -= 1;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Counting(usize);

impl Write for Counting {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += 1;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn an_extract_round_trips_through_its_file_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("facts.jsonl.gz");
    let facts = sample();
    write(&path, &facts).unwrap();
    assert_eq!(read(&path).unwrap(), facts);

    let mut first = Vec::new();
    encode(&mut first, &facts).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), first);
    let mut second = Vec::new();
    encode(&mut second, &facts).unwrap();
    assert_eq!(first, second);
}

#[test]
fn an_empty_extract_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("facts.gz");
    assert!(matches!(write(&path, &[]), Err(WriteError::Empty)));
    assert!(!path.exists());

    std::fs::write(&path, "existing output").unwrap();
    assert!(matches!(write(&path, &[]), Err(WriteError::Empty)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing output");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn a_new_extract_replaces_the_old_one_and_leaves_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("facts.gz");
    std::fs::write(&path, "old").unwrap();
    write(&path, &sample()).unwrap();
    assert_eq!(read(&path).unwrap(), sample());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn a_destination_that_cannot_be_replaced_is_an_error_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("occupied");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep"), "kept").unwrap();
    assert!(matches!(
        write(&path, &sample()),
        Err(WriteError::Replace { .. })
    ));
    assert_eq!(std::fs::read_to_string(path.join("keep")).unwrap(), "kept");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

    let missing = dir.path().join("absent").join("facts.gz");
    assert!(matches!(write(&missing, &sample()), Err(WriteError::Io(_))));
    assert!(!missing.exists());
}

#[test]
fn a_write_failure_at_any_step_is_reported() {
    let facts = sample();
    let mut counting = Counting(0);
    encode(&mut counting, &facts).unwrap();
    assert!(counting.0 >= 2);
    for accepted in 0..counting.0 {
        let error = encode(Failing { accepted }, &facts).unwrap_err();
        assert!(
            matches!(&error, WriteError::Io(source) if source.to_string().contains("rejected")),
            "after {accepted} writes: {error}"
        );
    }
}

#[test]
fn a_voltage_request_that_is_not_finite_is_refused() {
    let mut facts = sample();
    facts[1].voltage_requests_v = [(core(0), f64::NAN)].into_iter().collect::<PerCore<_>>();
    assert!(matches!(
        encode(Vec::new(), &facts),
        Err(WriteError::NotFinite { index: 1 })
    ));
}

#[test]
fn an_unreadable_extract_is_an_error_and_never_a_partial_result() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        read(dir.path().join("missing.gz")),
        Err(ReadError::Open { .. })
    ));

    let valid = line(&sample()[0]);
    let mut truncated = gz(&format!("{valid}\n"));
    truncated.truncate(truncated.len() - 8);
    let error = decode(truncated.as_slice()).unwrap_err();
    assert!(
        matches!(&error, ReadError::Io(source) if source.kind() == io::ErrorKind::UnexpectedEof),
        "truncated gzip: {error}"
    );
    let mut trailing = gz(&format!("{valid}\n"));
    trailing.extend_from_slice(b"not a gzip member");
    for (name, bytes) in [
        ("not gzip", b"not compressed".to_vec()),
        ("trailing garbage", trailing),
    ] {
        assert!(
            matches!(decode(bytes.as_slice()), Err(ReadError::Io(_))),
            "{name}"
        );
    }
}

#[test]
fn an_extract_is_one_valid_record_per_line() {
    let valid = line(&sample()[0]);
    for (name, text, bad_line) in [
        (
            "malformed second line",
            format!("{valid}\n{{invalid}}\n"),
            2,
        ),
        ("empty record", "{}\n".to_owned(), 1),
        ("blank line", format!("{valid}\n\n{valid}\n"), 2),
        ("two records on one line", format!("{valid}{valid}\n"), 1),
        ("a record array", format!("[{valid}]\n"), 1),
    ] {
        match decode(gz(&text).as_slice()) {
            Err(ReadError::Line { line, .. }) => assert_eq!(line, bad_line, "{name}"),
            other => panic!("{name}: {other:?}"),
        }
    }
    assert_eq!(
        decode(gz(&format!("{valid}\n{valid}")).as_slice())
            .unwrap()
            .len(),
        2,
        "the last line needs no newline"
    );
}

#[test]
fn an_extract_without_records_is_refused() {
    assert!(matches!(decode(gz("").as_slice()), Err(ReadError::Empty)));
}

#[test]
fn a_record_that_contradicts_itself_or_the_vocabulary_is_refused() {
    let facts = sample();
    let pass = serde_json::to_value(&facts[0]).unwrap();
    let failure = serde_json::to_value(&facts[1]).unwrap();
    let idle = serde_json::to_value(&facts[2]).unwrap();
    let edit = |base: &serde_json::Value, change: &dyn Fn(&mut serde_json::Value)| {
        let mut value = base.clone();
        change(&mut value);
        value
    };
    let cases = [
        (
            "ruleset differs from the build",
            edit(&pass, &|v| v["ruleset"] = 9.into()),
        ),
        (
            "a private key",
            edit(&pass, &|v| v["boot"] = "private-boot".into()),
        ),
        (
            "a private key in the context",
            edit(&pass, &|v| v["context"]["host"] = "private".into()),
        ),
        (
            "a pass with a signal",
            edit(&pass, &|v| v["signal"] = "crash".into()),
        ),
        (
            "a failure without a signal",
            edit(&failure, &|v| {
                v.as_object_mut().unwrap().remove("signal");
            }),
        ),
        (
            "a trial without a workload",
            edit(&pass, &|v| {
                v["class"].as_object_mut().unwrap().remove("workload");
            }),
        ),
        (
            "a regime that is not the workload's",
            edit(&pass, &|v| v["class"]["regime"] = "R2".into()),
        ),
        (
            "a workload outside the catalog",
            edit(&pass, &|v| v["class"]["workload"] = "nonesuch".into()),
        ),
        (
            "an idle failure naming a workload",
            edit(&idle, &|v| {
                v["class"]["workload"] = "mprime-sse-4k-21k".into();
            }),
        ),
        (
            "an idle pass",
            edit(&idle, &|v| {
                v["outcome"] = "pass".into();
                v.as_object_mut().unwrap().remove("signal");
            }),
        ),
        (
            "an offset below the clamp",
            edit(&pass, &|v| v["profile"][0] = (-51).into()),
        ),
        (
            "a positive offset",
            edit(&pass, &|v| v["profile"][0] = 1.into()),
        ),
        (
            "no profile",
            edit(&pass, &|v| v["profile"] = serde_json::json!([])),
        ),
        (
            "seventeen offsets",
            edit(&pass, &|v| {
                v["profile"] = serde_json::Value::from(vec![0; 17]);
            }),
        ),
        (
            "a repeated core",
            edit(&pass, &|v| v["class"]["cores"] = serde_json::json!([0, 0])),
        ),
        (
            "a core that does not exist",
            edit(&failure, &|v| v["core"] = 16.into()),
        ),
        ("a seq of zero", edit(&pass, &|v| v["seq"] = 0.into())),
        (
            "an unknown signal",
            edit(&failure, &|v| v["signal"] = "reboot".into()),
        ),
        (
            "an unknown condition",
            edit(&pass, &|v| v["condition"] = "isolated".into()),
        ),
        (
            "an unknown phase",
            edit(&pass, &|v| v["phase"] = "guard".into()),
        ),
        (
            "a malformed session",
            edit(&pass, &|v| v["session"] = "yesterday".into()),
        ),
    ];
    for (name, value) in cases {
        let text = format!("{value}\n");
        assert!(
            matches!(
                decode(gz(&text).as_slice()),
                Err(ReadError::Line { line: 1, .. })
            ),
            "{name}"
        );
    }
    let ok = format!("{pass}\n{failure}\n{idle}\n");
    assert_eq!(decode(gz(&ok).as_slice()).unwrap(), facts);
}
