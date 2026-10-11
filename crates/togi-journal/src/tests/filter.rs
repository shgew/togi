use std::str::FromStr;

use jiff::Timestamp;

use super::fixtures::{at, core, end, intent, sample, set};
use crate::*;

fn selector(text: &str) -> KindSelector {
    text.parse().unwrap()
}

fn seqs(events: &[&Event]) -> Vec<u64> {
    events.iter().map(|event| event.seq.get()).collect()
}

fn failure_event(seq_no: u64, seconds: i64, trial: Option<&str>, core_id: Option<u8>, known: Option<u64>) -> Event {
    let Payload::Failure(mut failure) = sample(EventKind::Failure) else {
        unreachable!()
    };
    failure.trial = trial.map(|id| TrialId::from(id.to_owned()));
    failure.core = core_id.map(core);
    failure.known_failure = known.and_then(Seq::new);
    at(seq_no, "boot-a", seconds, &[], failure.into())
}

fn journal() -> Vec<Event> {
    vec![
        at(1, "boot-a", 0, &[], sample(EventKind::SessionStart)),
        at(2, "boot-a", 10, &[], intent("0001", Some(3), &[])),
        at(3, "boot-a", 20, &[2], end("0001", None)),
        at(4, "boot-a", 30, &[], intent("0002", None, &[1, 2])),
        failure_event(5, 40, Some("0002"), None, None),
        at(6, "boot-a", 50, &[4], end("0002", None)),
        at(7, "boot-a", 60, &[], intent("0003", Some(5), &[])),
        at(8, "boot-a", 70, &[], end("0003", Some(5))),
    ]
}

fn filter(cores: &[u8]) -> Filter {
    Filter {
        cores: set(cores),
        ..Filter::default()
    }
}

#[test]
fn an_empty_filter_selects_everything() {
    let events = journal();
    assert_eq!(Filter::default().select(&events).len(), events.len());
}

#[test]
fn kind_and_time_intersect_with_inclusive_since_and_exclusive_until() {
    let events = journal();
    let since: Timestamp = "2026-10-02T01:00:20Z".parse().unwrap();
    let until: Timestamp = "2026-10-02T01:00:50Z".parse().unwrap();
    let kinds = Filter {
        kinds: vec![selector("trial")],
        since: Some(since),
        until: Some(until),
        ..Filter::default()
    };
    assert_eq!(seqs(&kinds.select(&events)), [3, 4]);
}

#[test]
fn a_group_selector_matches_dotted_kinds_and_an_exact_selector_only_itself() {
    let failure = failure_event(1, 0, None, None, None);
    let only_carried = Filter {
        kinds: vec![selector("failure.carried")],
        ..Filter::default()
    };
    assert!(selector("failure").matches(EventKind::FailureCarried));
    assert!(selector("failure").matches(EventKind::Failure));
    assert!(!selector("failure.carried").matches(EventKind::Failure));
    assert!(!only_carried.matches(&failure));
    assert!(selector("trial").matches(EventKind::TrialEnd));
    assert!(!selector("trial").matches(EventKind::Failure));
}

#[test]
fn selectors_validate_against_the_registry() {
    let names = KindSelector::names();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    for name in ["trial", "trial.end", "failure", "failure.carried", "deadend", "mce", "session", "journal"] {
        assert!(names.contains(&name), "{name}");
        assert!(KindSelector::from_str(name).is_ok(), "{name}");
    }
    let unknown = KindSelector::from_str("trial.nope").unwrap_err();
    assert!(unknown.to_string().contains("valid names:"), "{unknown}");
    assert!(unknown.to_string().contains("trial.end"), "{unknown}");
    assert!(KindSelector::from_str("nope").is_err());
    assert_eq!(KindSelector::from_str(""), Err(InvalidKindSelector::Empty));
}

#[test]
fn a_core_filter_names_events_and_widens_to_the_trials_that_load_the_core() {
    let events = journal();
    assert_eq!(seqs(&filter(&[3]).select(&events)), [2, 3]);
    assert_eq!(seqs(&filter(&[1]).select(&events)), [4, 5, 6]);
    assert_eq!(seqs(&filter(&[5]).select(&events)), [7, 8]);
}

#[test]
fn session_wide_events_stay_out_of_a_core_filter() {
    let events = journal();
    assert!(!seqs(&filter(&[3]).select(&events)).contains(&1));
}

#[test]
fn a_trial_that_started_before_since_still_counts() {
    let events = journal();
    let widened = Filter {
        cores: set(&[3]),
        since: Some("2026-10-02T01:00:15Z".parse().unwrap()),
        ..Filter::default()
    };
    assert_eq!(seqs(&widened.select(&events)), [3]);
}

#[test]
fn match_alone_lacks_the_lifecycle_widening() {
    let events = journal();
    let by_core = filter(&[3]);
    assert!(by_core.matches(&events[1]));
    assert!(!by_core.matches(&events[2]));
}

#[test]
fn an_unattributed_failure_of_a_loading_trial_matches_but_a_core_naming_one_does_not_widen() {
    let events = journal();
    assert!(seqs(&filter(&[2]).select(&events)).contains(&5));
    let mut named = journal();
    named[4] = failure_event(5, 40, Some("0002"), Some(7), None);
    assert!(!seqs(&filter(&[2]).select(&named)).contains(&5));
    assert!(seqs(&filter(&[7]).select(&named)).contains(&5));
}

#[test]
fn a_known_failure_follows_the_event_it_names_not_its_trial_id() {
    let mut events = journal();
    events.push(failure_event(9, 80, Some("0002"), None, Some(2)));
    events.push(failure_event(10, 90, Some("0001"), None, Some(5)));
    let selected = seqs(&filter(&[3]).select(&events));
    assert!(selected.contains(&9), "{selected:?}");
    assert!(!selected.contains(&10), "{selected:?}");
}

#[test]
fn a_known_failure_matches_even_when_its_source_lies_outside_the_window() {
    let mut events = journal();
    events.push(failure_event(9, 80, Some("0002"), None, Some(2)));
    let narrowed = Filter {
        cores: set(&[3]),
        since: Some("2026-10-02T01:01:10Z".parse().unwrap()),
        ..Filter::default()
    };
    assert_eq!(seqs(&narrowed.select(&events)), [9]);
}

#[test]
fn trial_filter_matches_trial_ids_and_carried_source_trials() {
    let mut events = journal();
    events.push(at(9, "boot-a", 80, &[], sample(EventKind::TrialCarried)));
    let by_trial = Filter {
        trial: Some(TrialId::from("0413".to_owned())),
        ..Filter::default()
    };
    assert_eq!(seqs(&by_trial.select(&events)), [9]);
    let by_trial = Filter {
        trial: Some(TrialId::from("0002".to_owned())),
        ..Filter::default()
    };
    assert_eq!(seqs(&by_trial.select(&events)), [4, 5, 6]);
}

#[test]
fn core_and_trial_filters_intersect() {
    let events = journal();
    let both = Filter {
        cores: set(&[1]),
        trial: Some(TrialId::from("0002".to_owned())),
        kinds: vec![selector("trial.end")],
        ..Filter::default()
    };
    assert_eq!(seqs(&both.select(&events)), [6]);
}

#[test]
fn core_filters_see_typed_core_fields_of_every_shape() {
    let cases = [
        (EventKind::TunerDecision, 7),
        (EventKind::CorePhase, 7),
        (EventKind::SmuIntent, 3),
        (EventKind::SmuReadback, 3),
        (EventKind::Mce, 3),
        (EventKind::CommandReset, 4),
        (EventKind::HuntStart, 3),
        (EventKind::HuntEnd, 1),
        (EventKind::Combination, 3),
        (EventKind::TrialCarried, 3),
        (EventKind::FailureCarried, 3),
        (EventKind::DeadEnd, 2),
        (EventKind::DefectFound, 2),
        (EventKind::SessionNotice, 1),
    ];
    for (kind, wanted) in cases {
        let event = at(1, "boot-a", 0, &[], sample(kind));
        assert!(filter(&[wanted]).matches(&event), "{kind} for core {wanted}");
        assert!(!filter(&[15]).matches(&event), "{kind} for core 15");
    }
}
