use std::num::NonZeroU32;

use jiff::Timestamp;
use proptest::prelude::*;

use crate::{InvalidSessionId, SessionId};

fn id(text: &str) -> SessionId {
    text.parse().unwrap()
}

#[test]
fn display_and_parse_agree() {
    for text in ["20261002T011407Z", "20261002T011407Z-2", "20261002T011407Z-10"] {
        assert_eq!(id(text).to_string(), text);
    }
}

#[test]
fn malformed_ids_are_refused() {
    for text in [
        "",
        "20261002T011407",
        "20261002 011407Z",
        "20261302T011407Z",
        "20261002T251407Z",
        "20261002T011407Z-",
        "20261002T011407Z-1",
        "20261002T011407Z-0",
        "20261002T011407Z-02",
        "20261002T011407Z-x",
        "20261002T011407Z-2-3",
        "../20261002T011407Z",
    ] {
        assert!(text.parse::<SessionId>().is_err(), "{text:?}");
    }
}

#[test]
fn order_is_time_then_numeric_suffix() {
    let ids = ["20261002T011407Z", "20261002T011407Z-2", "20261002T011407Z-10", "20261002T011408Z"];
    let mut sorted: Vec<_> = ids.iter().map(|text| id(text)).collect();
    sorted.reverse();
    sorted.sort();
    let names: Vec<_> = sorted.iter().map(ToString::to_string).collect();
    assert_eq!(names, ids);
    assert!(id("20261002T011407Z-9") < id("20261002T011407Z-10"));
}

#[test]
fn new_truncates_to_the_second_and_checks_the_suffix() {
    let time: Timestamp = "2026-10-02T01:14:07.987654321Z".parse().unwrap();
    let made = SessionId::new(time, None).unwrap();
    assert_eq!(made.to_string(), "20261002T011407Z");
    assert_eq!(made.timestamp().subsec_nanosecond(), 0);
    assert_eq!(
        SessionId::new(time, NonZeroU32::new(1)),
        Err(InvalidSessionId::Suffix(1))
    );
    assert_eq!(SessionId::new(Timestamp::MIN, None), Err(InvalidSessionId::Time));
}

#[test]
fn next_suffix_counts_and_stops_at_overflow() {
    let first = id("20261002T011407Z");
    let second = first.next_suffix().unwrap();
    assert_eq!(second.to_string(), "20261002T011407Z-2");
    assert_eq!(second.next_suffix().unwrap().to_string(), "20261002T011407Z-3");
    let last = SessionId::new(first.timestamp(), NonZeroU32::new(u32::MAX)).unwrap();
    assert_eq!(last.next_suffix(), None);
}

#[test]
fn serde_uses_the_text_form() {
    let made = id("20261002T011407Z-3");
    let json = serde_json::to_string(&made).unwrap();
    assert_eq!(json, "\"20261002T011407Z-3\"");
    assert_eq!(serde_json::from_str::<SessionId>(&json).unwrap(), made);
    assert!(serde_json::from_str::<SessionId>("\"nope\"").is_err());
}

proptest! {
    #[test]
    fn any_valid_id_round_trips(seconds in 0_i64..253_402_300_799, suffix in prop::option::of(2_u32..)) {
        let time = Timestamp::from_second(seconds).unwrap();
        let made = SessionId::new(time, suffix.and_then(NonZeroU32::new)).unwrap();
        prop_assert_eq!(made.to_string().parse::<SessionId>().unwrap(), made);
    }
}
