use std::collections::BTreeMap;
use std::io::{self, Write};

use flate2::Compression;
use flate2::write::GzEncoder;
use test_case::test_case;
use togi_facts::{Decisive, TrialClass};
use togi_machine::{CcdId, Condition, PerCore, Regime, Signal};
use togi_tool_trialfacts::{
    Class, Phase, ReadError, TrialFact, WriteError, decode, encode, read, write_unsynced,
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
fn an_extract_round_trips_through_its_bytes() {
    let facts = sample();
    let mut first = Vec::new();
    encode(&mut first, &facts).unwrap();
    assert_eq!(decode(first.as_slice()).unwrap(), facts);
    let mut second = Vec::new();
    encode(&mut second, &facts).unwrap();
    assert_eq!(first, second);
}

#[test]
fn an_empty_extract_changes_nothing() {
    let mut bytes = Vec::new();
    assert!(matches!(encode(&mut bytes, &[]), Err(WriteError::Empty)));
    assert!(bytes.is_empty());

    bytes.extend_from_slice(b"existing output");
    assert!(matches!(encode(&mut bytes, &[]), Err(WriteError::Empty)));
    assert_eq!(bytes, b"existing output");
}

#[test]
fn real_fs_a_new_extract_replaces_the_old_one_and_leaves_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("facts.gz");
    std::fs::write(&path, "old").unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the kernel publication test deliberately writes a disposable unsynced extract"
    )]
    write_unsynced(&path, &sample()).unwrap();
    assert_eq!(read(&path).unwrap(), sample());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn real_fs_a_destination_that_cannot_be_replaced_is_an_error_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("occupied");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep"), "kept").unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the kernel replacement failure test must not sync its disposable temporary file"
    )]
    let result = write_unsynced(&path, &sample());
    assert!(matches!(result, Err(WriteError::Replace { .. })));
    assert_eq!(std::fs::read_to_string(path.join("keep")).unwrap(), "kept");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

    let missing = dir.path().join("absent").join("facts.gz");
    #[expect(
        clippy::disallowed_methods,
        reason = "the kernel creation failure test uses the non-durable disk adapter"
    )]
    let result = write_unsynced(&missing, &sample());
    assert!(matches!(result, Err(WriteError::Io(_))));
    assert!(!missing.exists());
    assert!(matches!(read(&missing), Err(ReadError::Open { .. })));
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

const WIRE_KEYS: [&str; 20] = [
    "session",
    "build",
    "ruleset",
    "context",
    "seq",
    "trial",
    "kind",
    "class",
    "condition",
    "phase",
    "record_only",
    "profile",
    "outcome",
    "signal",
    "duration_s",
    "stalled_core",
    "core",
    "voltage_requests_v",
    "top_requesters",
    "ccd_mhz",
];

fn positional(object: &serde_json::Value, keys: &[&str]) -> serde_json::Value {
    let defaults = serde_json::json!({
        "voltage_requests_v": {}, "top_requesters": [], "ccd_mhz": {}, "record_only": false,
    });
    keys.iter()
        .map(|key| {
            object
                .get(key)
                .or_else(|| defaults.get(key))
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        })
        .collect()
}

#[test]
fn a_fact_that_would_not_read_back_is_refused_and_leaves_the_destination_unchanged() {
    let mut facts = sample();
    facts[2].outcome = Decisive::Pass;
    let mut bytes = b"existing output".to_vec();
    assert!(matches!(
        encode(&mut bytes, &facts),
        Err(WriteError::Invalid { index: 2, .. })
    ));
    assert_eq!(bytes, b"existing output");
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
fn a_truncated_gzip_is_an_unexpected_end() {
    let valid = line(&sample()[0]);
    let mut truncated = gz(&format!("{valid}\n"));
    truncated.truncate(truncated.len() - 8);
    let error = decode(truncated.as_slice()).unwrap_err();
    assert!(
        matches!(&error, ReadError::Io(source) if source.kind() == io::ErrorKind::UnexpectedEof),
        "{error}"
    );
}

fn not_gzip() -> Vec<u8> {
    b"not compressed".to_vec()
}

fn trailing_garbage() -> Vec<u8> {
    let mut trailing = gz(&format!("{}\n", line(&sample()[0])));
    trailing.extend_from_slice(b"not a gzip member");
    trailing
}

#[test_case(not_gzip; "not gzip")]
#[test_case(trailing_garbage; "trailing garbage")]
fn an_unreadable_extract_is_an_error_and_never_a_partial_result(bytes: fn() -> Vec<u8>) {
    assert!(matches!(decode(bytes().as_slice()), Err(ReadError::Io(_))));
}

#[test_case(|valid| format!("{valid}\n{{invalid}}\n"), 2; "malformed second line")]
#[test_case(|_| "{}\n".to_owned(), 1; "empty record")]
#[test_case(|valid| format!("{valid}\n\n{valid}\n"), 2; "blank line")]
#[test_case(|valid| format!("{valid}{valid}\n"), 1; "two records on one line")]
#[test_case(|valid| format!("[{valid}]\n"), 1; "a record array")]
fn an_extract_is_one_valid_record_per_line(text: fn(&str) -> String, bad_line: u64) {
    let valid = line(&sample()[0]);
    match decode(gz(&text(&valid)).as_slice()) {
        Err(ReadError::Line { line, .. }) => assert_eq!(line, bad_line),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_last_line_needs_no_newline() {
    let valid = line(&sample()[0]);
    assert_eq!(
        decode(gz(&format!("{valid}\n{valid}")).as_slice())
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn an_extract_without_records_is_refused() {
    assert!(matches!(decode(gz("").as_slice()), Err(ReadError::Empty)));
}

const PASS: usize = 0;
const FAILURE: usize = 1;
const IDLE: usize = 2;

#[test_case(PASS, |v| v["ruleset"] = 9.into(); "ruleset differs from the build")]
#[test_case(PASS, |v| v["boot"] = "private-boot".into(); "a private key")]
#[test_case(PASS, |v| v["context"]["host"] = "private".into(); "a private key in the context")]
#[test_case(PASS, |v| v["signal"] = "crash".into(); "a pass with a signal")]
#[test_case(FAILURE, |v| {
    v.as_object_mut().unwrap().remove("signal");
}; "a failure without a signal")]
#[test_case(PASS, |v| {
    v["class"].as_object_mut().unwrap().remove("workload");
}; "a trial without a workload")]
#[test_case(PASS, |v| v["class"]["regime"] = "R2".into(); "a regime that is not the workload's")]
#[test_case(PASS, |v| v["class"]["workload"] = "nonesuch".into(); "a workload outside the catalog")]
#[test_case(IDLE, |v| v["class"]["workload"] = "mprime-sse-4k-21k".into(); "an idle failure naming a workload")]
#[test_case(IDLE, |v| {
    v["outcome"] = "pass".into();
    v.as_object_mut().unwrap().remove("signal");
}; "an idle pass")]
#[test_case(PASS, |v| v["profile"][0] = (-51).into(); "an offset below the clamp")]
#[test_case(PASS, |v| v["profile"][0] = 1.into(); "a positive offset")]
#[test_case(PASS, |v| v["profile"] = serde_json::json!([]); "no profile")]
#[test_case(PASS, |v| v["profile"] = serde_json::Value::from(vec![0; 17]); "seventeen offsets")]
#[test_case(PASS, |v| v["class"]["cores"] = serde_json::json!([0, 0]); "a repeated core")]
#[test_case(FAILURE, |v| v["core"] = 16.into(); "a core that does not exist")]
#[test_case(PASS, |v| v["seq"] = 0.into(); "a seq of zero")]
#[test_case(FAILURE, |v| v["signal"] = "reboot".into(); "an unknown signal")]
#[test_case(PASS, |v| v["condition"] = "isolated".into(); "an unknown condition")]
#[test_case(PASS, |v| v["phase"] = "guard".into(); "an unknown phase")]
#[test_case(PASS, |v| v["session"] = "yesterday".into(); "a malformed session")]
#[test_case(IDLE, |v| v["class"]["duration_s"] = 90.into(); "an idle failure with a trial duration")]
#[test_case(PASS, |v| *v = positional(v, &WIRE_KEYS); "a record as a positional array")]
#[test_case(PASS, |v| {
    v["build"] = positional(
        &v["build"],
        &["version", "rev", "ruleset", "schema", "fixes", "evidence"],
    );
}; "a build stamp as a positional array")]
#[test_case(PASS, |v| {
    v["class"] = positional(&v["class"], &["regime", "workload", "cores", "duration_s"]);
}; "a class as a positional array")]
#[test_case(PASS, |v| {
    v["context"] = positional(
        &v["context"],
        &["bios_version", "board", "cpu_model", "microcode", "boost_limit_mhz"],
    );
}; "a context as a positional array")]
fn a_record_that_contradicts_itself_or_the_vocabulary_is_refused(
    base: usize,
    change: fn(&mut serde_json::Value),
) {
    let mut value = serde_json::to_value(&sample()[base]).unwrap();
    change(&mut value);
    let text = format!("{value}\n");
    assert!(matches!(
        decode(gz(&text).as_slice()),
        Err(ReadError::Line { line: 1, .. })
    ));
}

#[test]
fn a_pass_a_failure_and_an_idle_record_read_back() {
    let facts = sample();
    let [pass, failure, idle] =
        [PASS, FAILURE, IDLE].map(|index| serde_json::to_value(&facts[index]).unwrap());
    let ok = format!("{pass}\n{failure}\n{idle}\n");
    assert_eq!(decode(gz(&ok).as_slice()).unwrap(), facts);
}

#[expect(
    dead_code,
    reason = "compile-only negative control for the durability tripwire"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "removing the unsynced writer entry must leave this expectation unfulfilled"
)]
fn the_unsynced_writer_keeps_its_durability_tripwire(
    path: &std::path::Path,
    facts: &[TrialFact],
) -> Result<(), togi_tool_trialfacts::WriteError> {
    write_unsynced(path, facts)
}
