use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use test_case::test_case;

use super::{
    CRASH_LOOP_REBOOTS, CRASH_LOOP_WINDOW, DAY_ENTRY_VARIABLE, End, Entry, GrubSelection, Guard,
    Morning, NIGHT_VARIABLE, Night, NightError, SAVED_ENTRY_VARIABLE, Selection, Setup, guard,
};
use crate::{Bootloader, Environment, SavedEntry};

type Values = Rc<RefCell<BTreeMap<String, String>>>;

/// GRUB's environment block in memory, shared with the test.
struct Memory(Values);

impl Environment for Memory {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        Ok(self.0.borrow().get(name).cloned())
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        for (name, value) in values {
            self.0
                .borrow_mut()
                .insert((*name).to_owned(), (*value).to_owned());
        }
        Ok(())
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        for name in names {
            self.0.borrow_mut().remove(*name);
        }
        Ok(())
    }
}

/// What the real GRUB implementation does, which `togi-hardware` tests against `grub-editenv`: the day's
/// remembered entry is selected again, or `saved_entry` is unset, and the night record goes.
impl Bootloader for Memory {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        let mut values = self.0.borrow_mut();
        let before = values
            .get(SAVED_ENTRY_VARIABLE)
            .cloned()
            .unwrap_or_default();
        let day = values
            .get(DAY_ENTRY_VARIABLE)
            .filter(|day| !day.is_empty())
            .cloned();
        let after = if let Some(day) = day {
            values.insert(SAVED_ENTRY_VARIABLE.to_owned(), day.clone());
            day
        } else {
            values.remove(SAVED_ENTRY_VARIABLE);
            String::new()
        };
        values.remove(NIGHT_VARIABLE);
        Ok(SavedEntry { before, after })
    }
}

const TUNING: &str = "NixOS - togi";
const DAY: &str = "NixOS - Configuration 7";

fn selection(saved: Option<&str>) -> (GrubSelection, Values) {
    let values = Values::default();
    if let Some(saved) = saved {
        values
            .borrow_mut()
            .insert(SAVED_ENTRY_VARIABLE.to_owned(), saved.to_owned());
    }
    (
        GrubSelection::new(Box::new(Memory(Rc::clone(&values)))),
        values,
    )
}

fn saved(values: &Values) -> Option<String> {
    values.borrow().get(SAVED_ENTRY_VARIABLE).cloned()
}

fn tuning() -> Entry {
    Entry::try_from(TUNING.to_owned()).unwrap()
}

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).unwrap()
}

fn morning() -> Morning {
    "07:00".parse().unwrap()
}

const MINUTE: i64 = 60;
const HOUR: i64 = 3600;
const START: i64 = 1_800_000_000;

#[test_case(START + 8 * HOUR - 1, false; "a second before the morning")]
#[test_case(START + 8 * HOUR, true; "at the morning")]
#[test_case(START + 8 * HOUR + 1, true; "a second after the morning")]
#[test_case(START + 30 * HOUR, true; "a day late")]
fn the_morning_ends_a_night_at_its_deadline(now: i64, ended: bool) {
    let night = Night::new(at(START + 8 * HOUR));
    assert_eq!(
        night.ended(at(now)),
        ended.then_some(End::Morning {
            until: at(START + 8 * HOUR)
        })
    );
}

#[test]
fn the_deadline_survives_crash_reboots_and_ends_the_first_boot_after_it() {
    let (mut grub, values) = selection(Some("NixOS - Configuration 7"));
    grub.begin(&tuning(), &Night::new(at(START + 8 * HOUR)))
        .unwrap();
    for hours in [1, 2, 4, 7] {
        let now = at(START + hours * HOUR);
        let Guard::Tune(night) = guard(&mut grub, now, morning(), &TimeZone::UTC).unwrap() else {
            panic!("{hours} h: the night ended early");
        };
        assert_eq!(night.until(), at(START + 8 * HOUR), "{hours} h");
        assert_eq!(saved(&values).as_deref(), Some(TUNING), "{hours} h");
    }
    let late = at(START + 9 * HOUR);
    assert_eq!(
        guard(&mut grub, late, morning(), &TimeZone::UTC).unwrap(),
        Guard::Return(End::Morning {
            until: at(START + 8 * HOUR)
        })
    );
    assert_eq!(saved(&values).as_deref(), Some("NixOS - Configuration 7"));
    assert_eq!(grub.night().unwrap(), None);
}

#[test]
fn a_boot_before_the_deadline_with_the_clock_set_back_keeps_the_night() {
    let (mut grub, _) = selection(None);
    grub.begin(&tuning(), &Night::new(at(START + 8 * HOUR)))
        .unwrap();
    let back = at(START - 400 * HOUR);
    assert!(matches!(
        guard(&mut grub, back, morning(), &TimeZone::UTC).unwrap(),
        Guard::Tune(_)
    ));
}

fn boots(spacing: i64, count: usize) -> Night {
    let mut night = Night::new(at(START + 100 * HOUR));
    for index in 0..count {
        night.boot(at(START + spacing * i64::try_from(index).unwrap()));
    }
    night
}

fn window() -> i64 {
    CRASH_LOOP_WINDOW.as_secs()
}

fn last(night: &Night) -> Timestamp {
    *night.boots().last().unwrap()
}

#[test]
fn the_reboots_of_a_crash_loop_return_to_the_day_with_the_first_boot_named() {
    let night = boots(MINUTE, CRASH_LOOP_REBOOTS + 1);
    assert_eq!(
        night.ended(last(&night)),
        Some(End::CrashLoop { since: at(START) })
    );
}

#[test]
fn one_boot_fewer_is_not_a_crash_loop() {
    let night = boots(MINUTE, CRASH_LOOP_REBOOTS);
    assert_eq!(night.ended(last(&night)), None);
}

#[test]
fn a_crash_loop_spans_at_most_the_window() {
    let count = CRASH_LOOP_REBOOTS + 1;
    let gaps = i64::try_from(CRASH_LOOP_REBOOTS).unwrap();
    let inside = boots(window() / gaps, count);
    assert!(inside.ended(last(&inside)).is_some(), "exactly the window");
    let outside = boots(window() / gaps + 1, count);
    assert_eq!(outside.ended(last(&outside)), None, "a second more");
}

#[test]
fn only_the_newest_boots_count_so_slow_crashes_never_add_up() {
    let mut night = Night::new(at(START + 100 * HOUR));
    for index in 0..40 {
        night.boot(at(START + index * 10 * MINUTE));
    }
    assert_eq!(night.boots().len(), CRASH_LOOP_REBOOTS + 1);
    assert_eq!(night.ended(last(&night)), None);
}

#[test]
fn a_clock_set_back_cannot_hide_a_crash_loop() {
    let mut night = Night::new(at(START + 100 * HOUR));
    for second in [0, 60, 30, 20, 10, 5] {
        night.boot(at(START + second));
    }
    assert_eq!(
        night.ended(at(START + 70)),
        Some(End::CrashLoop { since: at(START) })
    );
}

#[test]
fn the_morning_is_named_before_a_crash_loop() {
    let mut night = Night::new(at(START));
    for boot in 0..=CRASH_LOOP_REBOOTS {
        night.boot(at(START + i64::try_from(boot).unwrap()));
    }
    assert_eq!(
        night.ended(at(START + HOUR)),
        Some(End::Morning { until: at(START) })
    );
}

#[test]
fn the_crash_loop_count_needs_no_journal_or_tuner_only_the_guard() {
    let (mut grub, values) = selection(Some("NixOS - Configuration 7"));
    grub.begin(&tuning(), &Night::new(at(START + 100 * HOUR)))
        .unwrap();
    let mut returned = None;
    for boot in 0..=CRASH_LOOP_REBOOTS {
        let now = at(START + i64::try_from(boot).unwrap() * MINUTE);
        match guard(&mut grub, now, morning(), &TimeZone::UTC).unwrap() {
            Guard::Tune(_) => assert_eq!(saved(&values).as_deref(), Some(TUNING)),
            Guard::Return(end) => returned = Some((boot, end)),
        }
    }
    assert_eq!(
        returned,
        Some((CRASH_LOOP_REBOOTS, End::CrashLoop { since: at(START) }))
    );
    assert_eq!(saved(&values).as_deref(), Some("NixOS - Configuration 7"));
}

#[test]
fn beginning_a_night_selects_the_tuning_entry_and_remembers_the_day_exactly() {
    let (mut grub, values) = selection(Some("Old>NixOS - Configuration 3 (2026-10-01)"));
    grub.begin(&tuning(), &Night::new(at(START + HOUR)))
        .unwrap();
    assert_eq!(saved(&values).as_deref(), Some(TUNING));
    assert_eq!(
        values.borrow().get(DAY_ENTRY_VARIABLE).map(String::as_str),
        Some("Old>NixOS - Configuration 3 (2026-10-01)")
    );
    let entry = grub.end().unwrap();
    assert_eq!(
        (entry.before.as_str(), entry.after.as_str()),
        (TUNING, "Old>NixOS - Configuration 3 (2026-10-01)")
    );
    assert_eq!(
        saved(&values).as_deref(),
        Some("Old>NixOS - Configuration 3 (2026-10-01)")
    );
    assert!(!values.borrow().contains_key(NIGHT_VARIABLE));
}

#[test]
fn a_day_without_a_saved_selection_returns_to_none_and_never_pins_a_generation() {
    let (mut grub, values) = selection(None);
    grub.begin(&tuning(), &Night::new(at(START + HOUR)))
        .unwrap();
    assert_eq!(saved(&values).as_deref(), Some(TUNING));
    grub.end().unwrap();
    assert_eq!(saved(&values), None);
    assert!(!values.borrow().contains_key(DAY_ENTRY_VARIABLE));
}

#[test]
fn returning_twice_selects_the_same_day_entry_again() {
    let (mut grub, values) = selection(Some("NixOS - Configuration 7"));
    grub.begin(&tuning(), &Night::new(at(START + HOUR)))
        .unwrap();
    grub.end().unwrap();
    grub.end().unwrap();
    assert_eq!(saved(&values).as_deref(), Some("NixOS - Configuration 7"));
}

#[test]
fn beginning_again_in_the_night_keeps_the_day_the_first_beginning_remembered() {
    let (mut grub, values) = selection(Some("NixOS - Configuration 7"));
    grub.begin(&tuning(), &Night::new(at(START + HOUR)))
        .unwrap();
    grub.begin(&tuning(), &Night::new(at(START + 2 * HOUR)))
        .unwrap();
    assert_eq!(grub.night().unwrap().unwrap().until(), at(START + 2 * HOUR));
    grub.end().unwrap();
    assert_eq!(saved(&values).as_deref(), Some("NixOS - Configuration 7"));
}

#[test]
fn a_tuning_boot_entered_from_the_menu_gets_a_night_ending_at_the_next_morning() {
    let (mut grub, values) = selection(Some(TUNING));
    let zone = TimeZone::UTC;
    let evening = at(1_800_000_000);
    let Guard::Tune(night) = guard(&mut grub, evening, morning(), &zone).unwrap() else {
        panic!("a fresh night ended");
    };
    let expected = morning().next_after(evening, &zone).unwrap();
    assert_eq!(night.until(), expected);
    assert!(
        expected > evening && expected.duration_since(evening) <= SignedDuration::from_hours(24)
    );
    assert_eq!(grub.night().unwrap(), Some(night));
    grub.end().unwrap();
    assert_eq!(saved(&values), None, "the day is the boot loader's default");
}

#[test]
fn a_stale_day_entry_is_dropped_when_a_menu_entered_night_is_adopted() {
    let (mut grub, values) = selection(Some(TUNING));
    values
        .borrow_mut()
        .insert(DAY_ENTRY_VARIABLE.to_owned(), "Stale".to_owned());
    guard(&mut grub, at(START), morning(), &TimeZone::UTC).unwrap();
    grub.end().unwrap();
    assert_eq!(saved(&values), None);
}

#[test]
fn a_damaged_record_is_replaced_by_a_night_to_the_next_morning_keeping_the_day() {
    let (mut grub, values) = selection(Some(TUNING));
    values
        .borrow_mut()
        .insert(DAY_ENTRY_VARIABLE.to_owned(), DAY.to_owned());
    values
        .borrow_mut()
        .insert(NIGHT_VARIABLE.to_owned(), "{\"until\":".to_owned());
    let Guard::Tune(night) = guard(&mut grub, at(START), morning(), &TimeZone::UTC).unwrap() else {
        panic!("a damaged record must not end the night");
    };
    assert_eq!(night.boots().len(), 1);
    assert!(grub.night().unwrap().is_some());
    grub.end().unwrap();
    assert_eq!(saved(&values).as_deref(), Some(DAY));
}

#[test_case("07:00", 7, 0; "seven")]
#[test_case("00:00", 0, 0; "midnight")]
#[test_case("23:59", 23, 59; "last minute")]
fn a_morning_reads_hours_and_minutes(text: &str, hour: i8, minute: i8) {
    let morning: Morning = text.parse().unwrap();
    assert_eq!(
        (morning.time().hour(), morning.time().minute()),
        (hour, minute)
    );
    assert_eq!(morning.to_string(), text);
}

#[test_case("7:00"; "one digit hour")]
#[test_case("24:00"; "hour out of range")]
#[test_case("07:60"; "minute out of range")]
#[test_case("07"; "no minutes")]
#[test_case("07:00:00"; "seconds")]
#[test_case("+7:00"; "sign")]
#[test_case(""; "empty")]
fn a_morning_refuses_other_text(text: &str) {
    assert!(text.parse::<Morning>().is_err(), "{text:?}");
}

#[test_case("2026-10-24T23:00:00Z", "2026-10-25T07:00:00Z"; "the evening before")]
#[test_case("2026-10-25T06:59:59Z", "2026-10-25T07:00:00Z"; "just before")]
#[test_case("2026-10-25T07:00:00Z", "2026-10-26T07:00:00Z"; "exactly at it asks for the next")]
fn the_next_morning_is_after_now(now: &str, expected: &str) {
    let found = morning()
        .next_after(now.parse().unwrap(), &TimeZone::UTC)
        .unwrap();
    assert_eq!(found, expected.parse::<Timestamp>().unwrap());
}

#[test]
fn the_next_morning_follows_the_zone() {
    let zone = TimeZone::fixed(jiff::tz::offset(2));
    let found = morning()
        .next_after("2026-10-24T23:00:00Z".parse().unwrap(), &zone)
        .unwrap();
    assert_eq!(found, "2026-10-25T05:00:00Z".parse::<Timestamp>().unwrap());
}

#[test]
fn reading_a_damaged_record_refuses_it_with_its_text() {
    let (mut grub, values) = selection(Some(TUNING));
    values
        .borrow_mut()
        .insert(NIGHT_VARIABLE.to_owned(), "{\"until\":".to_owned());
    let error = grub.night().unwrap_err();
    assert!(
        matches!(&error, NightError::Invalid { record, .. } if record == "{\"until\":"),
        "{error:?}"
    );
    assert_eq!(saved(&values).as_deref(), Some(TUNING));
}

#[test]
fn a_record_of_too_many_boots_is_refused() {
    let (mut grub, values) = selection(Some(TUNING));
    values.borrow_mut().insert(
        NIGHT_VARIABLE.to_owned(),
        r#"{"until":1,"boots":[1,2,3,4,5,6,7]}"#.to_owned(),
    );
    assert!(matches!(grub.night(), Err(NightError::Invalid { .. })));
}

#[test]
fn the_setup_names_the_environment_the_entry_and_the_morning() {
    let setup: Setup =
        r#"{"grubenv":"/boot/grub/grubenv","entry":"NixOS - togi","morning":"06:30"}"#
            .parse()
            .unwrap();
    assert_eq!(setup.grubenv.to_str(), Some("/boot/grub/grubenv"));
    assert_eq!(setup.entry.as_str(), TUNING);
    assert_eq!(setup.morning.to_string(), "06:30");
}

#[test_case(r#"{"grubenv":"/g","entry":"","morning":"06:30"}"#; "empty entry")]
#[test_case(r#"{"grubenv":"/g","entry":"a","morning":"6:30"}"#; "bad morning")]
#[test_case(r#"{"grubenv":"/g","entry":"a","morning":"06:30","extra":1}"#; "unknown field")]
#[test_case(r#"{"entry":"a","morning":"06:30"}"#; "missing grubenv")]
fn a_malformed_setup_is_refused(text: &str) {
    assert!(text.parse::<Setup>().is_err(), "{text}");
}
