use std::collections::BTreeMap;
use std::error::Error as _;
use std::io;

use proptest::prelude::*;
use test_case::test_case;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Get,
    Set,
    Unset,
    Clear,
}

struct MemoryBootloader {
    values: BTreeMap<String, String>,
    log: Vec<String>,
    sets: usize,
    cleared: usize,
    failing: Vec<Op>,
}

impl MemoryBootloader {
    fn new(values: &[(&str, &str)]) -> Self {
        Self {
            values: values
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            log: Vec::new(),
            sets: 0,
            cleared: 0,
            failing: Vec::new(),
        }
    }

    fn failing(mut self, ops: &[Op]) -> Self {
        self.failing = ops.to_vec();
        self
    }

    fn fail(&self, op: Op) -> io::Result<()> {
        if self.failing.contains(&op) {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "GRUB failure",
            ))
        } else {
            Ok(())
        }
    }

    fn snapshot(&self) -> Vec<(&str, &str)> {
        self.values
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }
}

impl Environment for MemoryBootloader {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        self.log.push(format!("get {name}"));
        self.fail(Op::Get)?;
        Ok(self.values.get(name).cloned())
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        self.log.push("set".to_owned());
        self.sets += 1;
        self.fail(Op::Set)?;
        for (name, value) in values {
            self.values.insert((*name).to_owned(), (*value).to_owned());
        }
        Ok(())
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        self.log.push(format!("unset {}", names.join(" ")));
        self.fail(Op::Unset)?;
        for name in names {
            self.values.remove(*name);
        }
        Ok(())
    }
}

impl Bootloader for MemoryBootloader {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        self.log.push("clear saved_entry".to_owned());
        self.cleared += 1;
        self.fail(Op::Clear)?;
        let before = self.values.remove("saved_entry").unwrap_or_default();
        Ok(SavedEntry {
            before,
            after: String::new(),
        })
    }
}

fn count(value: u8) -> RestartCount {
    RestartCount::try_from(value).unwrap()
}

fn io_source(error: &Error) -> io::ErrorKind {
    error
        .source()
        .and_then(|source| source.downcast_ref::<io::Error>())
        .map(io::Error::kind)
        .unwrap()
}

fn printable(value: &str) -> bool {
    value.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}

#[test]
fn two_retries_then_exhaustion_clears_saved_entry_and_stays_at_three() {
    let mut bootloader = MemoryBootloader::new(&[("saved_entry", "togi")]);
    let mut previous_id = None;
    for attempt in 1..=4_u8 {
        bootloader.log.clear();
        let limit = restart_limit(&mut bootloader).unwrap();
        let wanted = attempt.min(3);
        let exhausted = attempt >= 3;
        let want_limit = if exhausted {
            RestartLimit::Exhausted
        } else {
            RestartLimit::Retry(count(wanted))
        };
        assert_eq!(limit, want_limit, "attempt {attempt}");
        assert_eq!(bootloader.sets, usize::from(attempt));
        let mut want_log = vec!["get togi_restart_count", "set"];
        if exhausted {
            want_log.push("clear saved_entry");
        }
        assert_eq!(bootloader.log, want_log, "attempt {attempt}");
        assert_eq!(
            bootloader.values[COUNT_VARIABLE],
            wanted.to_string(),
            "attempt {attempt}"
        );
        let record = read_reason(&mut bootloader).unwrap().unwrap();
        assert_ne!(Some(record.id.clone()), previous_id);
        assert_eq!(record.count, count(wanted));
        assert_eq!(
            record.reason.as_str(),
            if exhausted {
                EXHAUSTED_REASON
            } else {
                RETRY_REASON
            }
        );
        assert_eq!(record.id.as_str().len(), 32);
        assert!(
            record
                .id
                .as_str()
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        previous_id = Some(record.id);
    }
    assert_eq!(bootloader.cleared, 2);
    assert!(!bootloader.values.contains_key("saved_entry"));
}

#[test_case(None, 1; "unset")]
#[test_case(Some(""), 1; "empty")]
#[test_case(Some("0"), 1; "zero")]
#[test_case(Some("1"), 2; "one")]
#[test_case(Some("2"), 3; "two")]
#[test_case(Some("3"), 3; "three saturates")]
fn stored_counts_map_to_the_next_count(stored: Option<&str>, want: u8) {
    let values: Vec<_> = stored.map(|v| (COUNT_VARIABLE, v)).into_iter().collect();
    let mut bootloader = MemoryBootloader::new(&values);
    restart_limit(&mut bootloader).unwrap();
    assert_eq!(
        bootloader.values[COUNT_VARIABLE],
        want.to_string(),
        "{stored:?}"
    );
}

#[test_case("-1"; "negative")]
#[test_case("4"; "above the limit")]
#[test_case("01"; "leading zero")]
#[test_case("+1"; "plus sign")]
#[test_case("1\n"; "trailing newline")]
#[test_case(" 1"; "leading space")]
#[test_case("1 "; "trailing space")]
#[test_case("x"; "letter")]
#[test_case("garbage"; "word")]
#[test_case("0x1"; "hex")]
#[test_case("１"; "full-width digit")]
#[test_case(&"9".repeat(301); "overlong")]
fn invalid_counts_grant_no_retry_and_write_nothing(value: &str) {
    let mut bootloader = MemoryBootloader::new(&[(COUNT_VARIABLE, value), ("saved_entry", "togi")]);
    let error = restart_limit(&mut bootloader).unwrap_err();
    assert!(
        matches!(&error, Error::InvalidCount(seen) if seen == value),
        "{value:?}: {error:?}"
    );
    assert_eq!(bootloader.sets, 0, "{value:?}");
    assert_eq!(bootloader.cleared, 0, "{value:?}");
    assert_eq!(bootloader.log, ["get togi_restart_count"]);
    assert_eq!(
        bootloader.snapshot(),
        [("saved_entry", "togi"), (COUNT_VARIABLE, value)]
    );
}

#[test]
fn a_failure_at_each_step_grants_no_retry_and_leaves_only_what_happened() {
    let state = [(COUNT_VARIABLE, "2"), ("saved_entry", "togi")];

    let mut bootloader = MemoryBootloader::new(&state).failing(&[Op::Get]);
    let error = restart_limit(&mut bootloader).unwrap_err();
    assert!(matches!(error, Error::ReadCount(_)));
    assert_eq!(io_source(&error), io::ErrorKind::PermissionDenied);
    assert_eq!(bootloader.sets, 0);
    assert_eq!(bootloader.cleared, 0);
    assert_eq!(
        bootloader.snapshot(),
        [("saved_entry", "togi"), (COUNT_VARIABLE, "2")]
    );

    let mut bootloader = MemoryBootloader::new(&state).failing(&[Op::Set]);
    let error = restart_limit(&mut bootloader).unwrap_err();
    assert!(matches!(error, Error::WriteCountAndReason(_)));
    assert_eq!(io_source(&error), io::ErrorKind::PermissionDenied);
    assert_eq!(bootloader.cleared, 0);
    assert_eq!(
        bootloader.snapshot(),
        [("saved_entry", "togi"), (COUNT_VARIABLE, "2")]
    );

    let mut bootloader = MemoryBootloader::new(&state).failing(&[Op::Clear]);
    let error = restart_limit(&mut bootloader).unwrap_err();
    assert!(matches!(error, Error::ClearSavedEntry(_)));
    assert_eq!(io_source(&error), io::ErrorKind::PermissionDenied);
    assert_eq!(bootloader.values[COUNT_VARIABLE], "3");
    assert_eq!(bootloader.values["saved_entry"], "togi");
    let record = read_reason(&mut bootloader).unwrap().unwrap();
    assert_eq!(record.reason.as_str(), EXHAUSTED_REASON);
}

#[test]
fn a_retry_never_clears_the_saved_entry() {
    let mut bootloader = MemoryBootloader::new(&[("saved_entry", "togi")]).failing(&[Op::Clear]);
    assert_eq!(
        restart_limit(&mut bootloader).unwrap(),
        RestartLimit::Retry(count(1))
    );
    assert_eq!(bootloader.cleared, 0);
}

#[test_case("0")]
#[test_case("1")]
#[test_case("2")]
fn a_random_failure_grants_no_retry_and_touches_nothing(stored: &str) {
    let broken = getrandom::Error::new_custom(1);
    let mut bootloader =
        MemoryBootloader::new(&[(COUNT_VARIABLE, stored), ("saved_entry", "togi")]);
    let error = restart_limit_with(&mut bootloader, |_| Err(broken)).unwrap_err();
    assert!(
        matches!(&error, Error::Random(source) if *source == broken),
        "{stored}"
    );
    assert!(error.source().is_some());
    assert_eq!(bootloader.log, ["get togi_restart_count"], "{stored}");
    assert_eq!(bootloader.sets, 0, "{stored}");
    assert_eq!(bootloader.cleared, 0, "{stored}");
    assert_eq!(
        bootloader.snapshot(),
        [("saved_entry", "togi"), (COUNT_VARIABLE, stored)]
    );
}

#[test]
fn quotes_and_backslashes_in_a_reason_are_escaped_and_decode_back() {
    let record = LeaveReason::new(r#"say "hi" in C:\temp"#, count(1)).unwrap();
    let encoded = record.encode();
    assert!(encoded.contains(r#"say \"hi\" in C:\\temp"#), "{encoded}");
    assert_eq!(LeaveReason::decode(&encoded).unwrap(), record);
}

#[test]
fn restart_limit_works_through_a_trait_object() {
    let mut bootloader: Box<dyn Bootloader> = Box::new(MemoryBootloader::new(&[]));
    assert_eq!(
        restart_limit(bootloader.as_mut()).unwrap(),
        RestartLimit::Retry(count(1))
    );
    assert!(read_reason(bootloader.as_mut()).unwrap().is_some());
}

#[test]
fn reason_round_trips_through_the_environment_and_cleans_up_independently() {
    let mut bootloader = MemoryBootloader::new(&[(COUNT_VARIABLE, "2"), ("saved_entry", "togi")]);
    assert!(read_reason(&mut bootloader).unwrap().is_none());
    bootloader
        .values
        .insert(REASON_VARIABLE.to_owned(), String::new());
    assert!(read_reason(&mut bootloader).unwrap().is_none());

    let want = LeaveReason::new("journal incompatible", count(0)).unwrap();
    write_reason(&mut bootloader, &want).unwrap();
    assert_eq!(read_reason(&mut bootloader).unwrap(), Some(want));

    reset_count(&mut bootloader).unwrap();
    assert!(!bootloader.values.contains_key(COUNT_VARIABLE));
    assert!(bootloader.values.contains_key(REASON_VARIABLE));

    clear_reason(&mut bootloader).unwrap();
    assert_eq!(bootloader.snapshot(), [("saved_entry", "togi")]);
}

#[test]
fn failed_operations_preserve_state() {
    let record = LeaveReason::new("retry", count(2)).unwrap();
    let mut bootloader = MemoryBootloader::new(&[
        (COUNT_VARIABLE, "2"),
        (REASON_VARIABLE, "pending"),
    ])
    .failing(&[Op::Get, Op::Set, Op::Unset]);
    let errors = [
        read_reason(&mut bootloader).unwrap_err(),
        write_reason(&mut bootloader, &record).unwrap_err(),
        clear_reason(&mut bootloader).unwrap_err(),
        reset_count(&mut bootloader).unwrap_err(),
    ];
    assert!(matches!(errors[0], Error::ReadReason(_)));
    assert!(matches!(errors[1], Error::WriteReason(_)));
    assert!(matches!(errors[2], Error::ClearReason(_)));
    assert!(matches!(errors[3], Error::ResetCount(_)));
    for error in &errors {
        assert_eq!(io_source(error), io::ErrorKind::PermissionDenied);
    }
    assert_eq!(
        bootloader.snapshot(),
        [(REASON_VARIABLE, "pending"), (COUNT_VARIABLE, "2")]
    );
}

#[test]
fn error_display_names_the_operation_without_the_source_text() {
    let mut bootloader = MemoryBootloader::new(&[]).failing(&[Op::Get]);
    let error = read_reason(&mut bootloader).unwrap_err();
    assert_eq!(error.to_string(), "read leave reason");
    assert_eq!(
        Error::InvalidCount("x".to_owned()).to_string(),
        "invalid restart count \"x\": must be 0 to 3"
    );
}

#[test_case("retry", Some("retry"); "plain")]
#[test_case("  padded \n", Some("padded"); "padded")]
#[test_case("first\nsecond\rthird\ttab\0 ☃", Some("first second third tab? ?"); "control and non-ascii")]
#[test_case("\u{7f}\u{1b}[0m", Some("??[0m"); "escape sequence")]
#[test_case(&"x".repeat(200), Some(&"x".repeat(160)); "overlong is cut")]
#[test_case(&format!("{} b", "a".repeat(159)), Some(&"a".repeat(159)); "cut then trimmed")]
#[test_case("", None; "empty")]
#[test_case(" \n\t\r ", None; "whitespace only")]
fn sanitize_replaces_cuts_and_trims(input: &str, want: Option<&str>) {
    assert_eq!(
        ReasonText::sanitize(input).as_ref().map(ReasonText::as_str),
        want,
        "{input:?}"
    );
}

#[test_case(&"x".repeat(200); "overlong")]
#[test_case(&"\"".repeat(200); "all escaped")]
#[test_case(&"\\".repeat(200); "backslashes")]
#[test_case(&"\"\\<&>".repeat(40); "mixed")]
#[test_case("first\nsecond\rthird\ttab\0 ☃"; "multiline")]
#[test_case("journal incompatible"; "ordinary")]
fn encoded_records_fit_and_round_trip_for_hostile_reasons(text: &str) {
    let id = ReasonId::try_from("a".repeat(64)).unwrap();
    let reason = ReasonText::sanitize(text).unwrap();
    let record = LeaveReason {
        id,
        count: count(3),
        reason: reason.clone(),
    };
    let mut bootloader = MemoryBootloader::new(&[]);
    write_reason(&mut bootloader, &record).unwrap();
    let encoded = bootloader.values[REASON_VARIABLE].clone();
    assert!(encoded.len() <= 300 && printable(&encoded), "{encoded}");
    let got = read_reason(&mut bootloader).unwrap().unwrap();
    assert_eq!(got.id, record.id);
    assert_eq!(got.count, record.count);
    assert!(reason.as_str().starts_with(got.reason.as_str()));
    assert!(!got.reason.as_str().ends_with(' '));
    if encoded.len() < 300 - 2 {
        assert_eq!(got.reason, reason, "nothing needed cutting: {encoded}");
    }
}

#[test]
fn an_escaped_reason_is_cut_to_the_budget() {
    let record = LeaveReason {
        id: ReasonId::try_from("a".repeat(64)).unwrap(),
        count: count(3),
        reason: ReasonText::sanitize(&"\"".repeat(160)).unwrap(),
    };
    let encoded = record.encode();
    assert!(
        encoded.len() == 299 || encoded.len() == 300,
        "{}",
        encoded.len()
    );
    assert_eq!(
        LeaveReason::decode(&encoded).unwrap().reason.as_str().len(),
        102
    );
}

#[test]
fn decoding_accepts_a_well_formed_record() {
    let record = LeaveReason::decode(
        r#"{"id":"fallback-0a1b","count":0,"reason":"restart-limit command failed"}"#,
    )
    .unwrap();
    assert_eq!(record.id.as_str(), "fallback-0a1b");
    assert_eq!(record.count, count(0));
}

#[test_case("not JSON"; "not json")]
#[test_case("null"; "null")]
#[test_case(r#"["test",0,"retry"]"#; "compact array")]
#[test_case(r#"["test", 0, "retry"]"#; "spaced array")]
#[test_case("[]"; "empty array")]
#[test_case("{}"; "empty object")]
#[test_case("{\n}"; "newline object")]
#[test_case(""; "empty")]
#[test_case(r#"{"id":"test","reason":"retry"}"#; "missing count")]
#[test_case(r#"{"id":"test","count":0}"#; "missing reason")]
#[test_case(r#"{"count":0,"reason":"retry"}"#; "missing id")]
#[test_case(r#"{"id":"","count":0,"reason":"retry"}"#; "empty id")]
#[test_case(r#"{"id":"bad id","count":0,"reason":"retry"}"#; "space in id")]
#[test_case(r#"{"id":"test","count":-1,"reason":"retry"}"#; "negative count")]
#[test_case(r#"{"id":"test","count":4,"reason":"retry"}"#; "count above limit")]
#[test_case(r#"{"id":"test","count":1.5,"reason":"retry"}"#; "fractional count")]
#[test_case(r#"{"id":"test","count":"1","reason":"retry"}"#; "string count")]
#[test_case(r#"{"id":"test","count":0,"reason":""}"#; "empty reason")]
#[test_case(r#"{"id":"test","count":0,"reason":" retry"}"#; "leading space reason")]
#[test_case(r#"{"id":"test","count":0,"reason":"retry\nline"}"#; "newline reason")]
#[test_case(r#"{"id":"test","count":0,"reason":"\u2603"}"#; "non-ascii reason")]
#[test_case(r#"{"id":"test","count":0,"reason":"retry","extra":true}"#; "extra field")]
#[test_case(r#"{"id":"test","count":0,"reason":"retry"} {}"#; "trailing object")]
#[test_case(r#"{"id":"test","count":0,"reason":"retry"}x"#; "trailing text")]
#[test_case(r#"{"id":"test","id":"other","count":0,"reason":"retry"}"#; "duplicate id")]
#[test_case(r#"{"id":"test","count":0,"count":1,"reason":"retry"}"#; "duplicate count")]
#[test_case(r#"{"id":"test","count":0,"reason":"retry","reason":"other"}"#; "duplicate reason")]
#[test_case(&format!(r#"{{"id":"{}","count":0,"reason":"retry"}}"#, "a".repeat(65)); "overlong id")]
#[test_case(&format!(r#"{{"id":"test","count":0,"reason":"{}"}}"#, "a".repeat(161)); "overlong reason")]
#[test_case(&" ".repeat(301); "oversized record")]
#[test_case("{\"id\":\"test\",\"count\":0,\"reason\":\"a\tb\"}"; "tab reason")]
fn decoding_rejects_invalid_records(case: &str) {
    let error = LeaveReason::decode(case).unwrap_err();
    assert!(matches!(error, Error::InvalidRecord(_)), "{case:?}");
    let mut bootloader = MemoryBootloader::new(&[(REASON_VARIABLE, case)]);
    if !case.is_empty() {
        assert!(read_reason(&mut bootloader).is_err(), "{case:?}");
    }
}

#[test]
fn decoding_names_the_reason_a_record_is_invalid() {
    assert!(matches!(
        LeaveReason::decode(&" ".repeat(301)),
        Err(Error::InvalidRecord(RecordError::TooLong))
    ));
    assert!(matches!(
        LeaveReason::decode("{\"id\":\"t\",\n\"count\":0,\"reason\":\"r\"}"),
        Err(Error::InvalidRecord(RecordError::NotPrintable))
    ));
    assert!(matches!(
        LeaveReason::decode(r#"{"id":"t","count":0,"reason":"r","x":1}"#),
        Err(Error::InvalidRecord(RecordError::Syntax(_)))
    ));
}

#[test_case(""; "empty")]
#[test_case("bad\nID"; "newline")]
#[test_case("bad id"; "space")]
#[test_case("bad/id"; "slash")]
#[test_case("ünï"; "non-ascii")]
#[test_case(&"a".repeat(65); "overlong")]
fn reason_ids_refuse_invalid_text(id: &str) {
    assert!(id.parse::<ReasonId>().is_err(), "{id:?}");
    assert!(ReasonId::try_from(id.to_owned()).is_err(), "{id:?}");
}

#[test_case("a"; "single letter")]
#[test_case("A-z_0.9"; "all allowed punctuation")]
#[test_case("fallback-0123456789abcdef0123456789abcdef"; "fallback id")]
fn reason_ids_accept_valid_text(id: &str) {
    assert_eq!(id.parse::<ReasonId>().unwrap().as_str(), id);
}

#[test_case(4)]
#[test_case(5)]
#[test_case(255)]
fn restart_counts_refuse_values_above_three(value: u8) {
    assert!(RestartCount::try_from(value).is_err());
}

#[test_case(""; "empty")]
#[test_case(" "; "space")]
#[test_case(" \n\t "; "whitespace")]
#[test_case(" lead"; "leading space")]
#[test_case("trail "; "trailing space")]
#[test_case("line\nbreak"; "newline")]
#[test_case("☃"; "non-ascii")]
#[test_case(&"a".repeat(161); "overlong")]
fn reason_text_refuses_invalid_text(text: &str) {
    assert!(text.parse::<ReasonText>().is_err(), "{text:?}");
}

#[test]
fn reason_text_accepts_the_longest_text_and_new_refuses_blank() {
    assert!("a".repeat(160).parse::<ReasonText>().is_ok());
    assert!(matches!(
        LeaveReason::new(" \n\t ", count(0)),
        Err(Error::InvalidRecord(RecordError::Text))
    ));
}

#[test]
fn restart_count_next_saturates_at_three() {
    let steps: Vec<u8> = std::iter::successors(Some(RestartCount::ZERO), |c| Some(c.next()))
        .take(6)
        .map(RestartCount::get)
        .collect();
    assert_eq!(steps, [0, 1, 2, 3, 3, 3]);
}

fn fixed_config() -> ProptestConfig {
    ProptestConfig {
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x7475_6e69_6e67_626f),
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

fn check_new(text: &str, wanted: u8) {
    let result = LeaveReason::new(text, count(wanted));
    let Some(sanitized) = ReasonText::sanitize(text) else {
        assert!(matches!(
            result,
            Err(Error::InvalidRecord(RecordError::Text))
        ));
        return;
    };
    let record = result.unwrap();
    let encoded = record.encode();
    assert!(encoded.len() <= 300 && printable(&encoded), "{encoded}");
    let decoded = LeaveReason::decode(&encoded).unwrap();
    assert_eq!(decoded.id, record.id);
    assert_eq!(decoded.count, record.count);
    assert!(sanitized.as_str().starts_with(decoded.reason.as_str()));
    assert_eq!(decoded.encode(), encoded);
    if encoded.len() < 300 - 2 {
        assert_eq!(decoded, record);
    }
}

proptest! {
    #![proptest_config(fixed_config())]

    #[test]
    fn new_records_encode_within_bounds_and_decode(text in any::<String>(), wanted in 0_u8..=3) {
        check_new(&text, wanted);
    }

    #[test]
    fn escape_heavy_records_encode_within_bounds_and_decode(
        text in "[\"\\\\a <>&\n☃]{0,400}",
        wanted in 0_u8..=3,
    ) {
        check_new(&text, wanted);
    }
}

#[test]
fn serde_serialization_matches_encoding_and_display_prints_the_text() {
    let record = LeaveReason::new("journal incompatible", count(2)).unwrap();
    assert_eq!(serde_json::to_string(&record).unwrap(), record.encode());
    assert_eq!(record.id.to_string(), record.id.as_str());
    assert_eq!(record.reason.to_string(), "journal incompatible");
    assert_eq!(String::from(record.id.clone()), record.id.as_str());
    assert_eq!(String::from(record.reason.clone()), "journal incompatible");
    assert_eq!(u8::from(record.count), 2);
}
