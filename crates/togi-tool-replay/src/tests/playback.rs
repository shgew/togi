use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use togi_journal::Event;
use togi_machine::BootId;
use togi_watchtest::Scenario;

use crate::{Playback, Speed, offset, prefix};

const START: Timestamp = Timestamp::constant(1000, 0);
const SECOND: Duration = Duration::from_secs(1);

/// One recorded event's stamps: wall time since the first, boot and monotonic milliseconds.
type Stamp = (SignedDuration, &'static str, Option<u64>);

fn secs(seconds: i64) -> SignedDuration {
    SignedDuration::from_secs(seconds)
}

fn recorded(stamps: &[Stamp]) -> Vec<Event> {
    let mut events = Scenario::Search.events().unwrap();
    events.truncate(stamps.len());
    assert_eq!(events.len(), stamps.len());
    for (event, &(wall, boot, mono)) in events.iter_mut().zip(stamps) {
        event.time = START + wall;
        event.boot = BootId::from(boot.to_owned());
        event.mono = mono.map(Duration::from_millis);
    }
    events
}

fn at(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn speed(factor: f64) -> Speed {
    Speed::new(factor).unwrap()
}

#[track_caller]
fn assert_playback(
    playback: &mut Playback<'_>,
    elapsed: Duration,
    played: usize,
    clock: Timestamp,
) {
    let (got, shown) = playback.advance(elapsed);
    assert_eq!(got, played, "at elapsed {elapsed:?}: events played");
    assert_eq!(shown, clock, "at elapsed {elapsed:?}: display clock");
}

#[test]
fn a_wall_clock_correction_in_the_recording_costs_no_playback_time() {
    for jump in [secs(-60), secs(60)] {
        let events = recorded(&[
            (secs(0), "boot", Some(0)),
            (jump + secs(10), "boot", Some(10_000)),
            (jump + secs(20), "boot", Some(20_000)),
            (jump + secs(30), "boot", Some(30_000)),
        ]);
        let mut playback = Playback::new(&events, 1, speed(1.0));
        for (elapsed, played) in [
            (at(0), 1),
            (at(9), 1),
            (at(10), 2),
            (at(15), 2),
            (at(20).checked_sub(Duration::from_nanos(1)).unwrap(), 2),
            (at(20), 3),
            (at(35), 4),
        ] {
            let wall = if played > 1 {
                SignedDuration::try_from(elapsed).unwrap() + jump
            } else {
                SignedDuration::try_from(elapsed).unwrap()
            };
            assert_playback(&mut playback, elapsed, played, START + wall);
        }
    }
}

#[test]
fn a_monotonic_zero_is_a_stamp_not_a_missing_one() {
    let events = recorded(&[
        (secs(0), "boot", Some(0)),
        (secs(3600), "boot", Some(0)),
        (secs(3620), "boot", Some(20_000)),
    ]);
    let mut playback = Playback::new(&events, 1, speed(1.0));
    assert_playback(&mut playback, at(0), 2, events[1].time);
    assert_playback(&mut playback, at(1), 2, events[1].time + secs(1));
    assert_playback(&mut playback, at(20), 3, events[2].time);
}

#[test]
fn without_two_monotonic_stamps_of_one_known_boot_the_wall_clock_paces_the_gap() {
    let cases: [(Stamp, Stamp); 5] = [
        ((secs(0), "boot", None), (secs(10), "boot", None)),
        ((secs(0), "boot", None), (secs(10), "boot", Some(0))),
        ((secs(0), "boot", Some(0)), (secs(10), "boot", None)),
        (
            (secs(0), "before", Some(100_000)),
            (secs(10), "after", Some(0)),
        ),
        ((secs(0), "", Some(0)), (secs(10), "", Some(0))),
    ];
    for (before, after) in cases {
        let events = recorded(&[before, after]);
        let mut playback = Playback::new(&events, 1, speed(1.0));
        assert_playback(&mut playback, at(9), 1, events[0].time + secs(9));
        assert_playback(&mut playback, at(10), 2, events[1].time);
        assert_playback(&mut playback, at(12), 2, events[1].time + secs(2));
        assert_eq!(events.len(), 2);
    }
}

#[test]
fn a_backward_wall_clock_gap_is_free_and_the_next_interval_is_kept() {
    let cases: [(&str, [Stamp; 3]); 2] = [
        (
            "no monotonic stamps",
            [
                (secs(0), "boot", None),
                (secs(-60), "boot", None),
                (secs(-50), "boot", None),
            ],
        ),
        (
            "cross boot",
            [
                (secs(0), "before", Some(100_000)),
                (secs(-60), "after", Some(0)),
                (secs(-50), "after", Some(10_000)),
            ],
        ),
    ];
    for (_, stamps) in cases {
        let events = recorded(&stamps);
        let mut playback = Playback::new(&events, 1, speed(1.0));
        assert_playback(&mut playback, at(0), 2, events[1].time);
        assert_playback(&mut playback, at(9), 2, events[1].time + secs(9));
        assert_playback(&mut playback, at(10), 3, events[2].time);
    }
}

#[test]
fn a_monotonic_clock_that_ran_backwards_within_a_boot_is_free_too() {
    let events = recorded(&[
        (secs(0), "boot", Some(50_000)),
        (secs(5), "boot", Some(40_000)),
        (secs(15), "boot", Some(50_000)),
    ]);
    let mut playback = Playback::new(&events, 1, speed(1.0));
    assert_playback(&mut playback, at(0), 2, events[1].time);
    assert_playback(&mut playback, at(9), 2, events[1].time + secs(9));
    assert_playback(&mut playback, at(10), 3, events[2].time);
}

#[test]
fn playback_starts_at_the_prefix_a_sequence_number_names() {
    let events = recorded(&[
        (secs(0), "boot", Some(0)),
        (secs(-50), "boot", Some(10_000)),
        (secs(80), "boot", Some(20_000)),
    ]);
    let mut playback = Playback::new(&events, prefix(&events, 2), speed(1.0));
    assert_playback(&mut playback, at(0), 2, events[1].time);
    assert_playback(&mut playback, at(9), 2, events[1].time + secs(9));
    assert_playback(&mut playback, at(10), 3, events[2].time);
    assert_playback(&mut playback, at(12), 3, events[2].time + secs(2));
}

#[test]
fn the_prefix_holds_the_events_up_to_a_sequence_number_and_at_least_the_first() {
    let events = recorded(&[
        (secs(0), "boot", None),
        (secs(1), "boot", None),
        (secs(2), "boot", None),
    ]);
    assert_eq!(prefix(&events, 0), 1);
    assert_eq!(prefix(&events, 1), 1);
    assert_eq!(prefix(&events, 2), 2);
    assert_eq!(prefix(&events, 3), 3);
    assert_eq!(prefix(&events, u64::MAX), 3);
}

#[test]
fn the_speed_scales_real_time_and_saturates_instead_of_overflowing() {
    let events = recorded(&[
        (secs(0), "boot", Some(0)),
        (secs(10), "boot", Some(10_000)),
        (secs(20), "boot", Some(20_000)),
    ]);
    let cases = [
        (0.5, at(30), 2, secs(15)),
        (300.0, Duration::from_millis(50), 2, secs(15)),
        (f64::MAX, Duration::ZERO, 1, secs(0)),
    ];
    for (factor, elapsed, played, wall) in cases {
        let mut playback = Playback::new(&events, 1, speed(factor));
        assert_playback(&mut playback, elapsed, played, START + wall);
    }
    for elapsed in [Duration::from_nanos(1), SECOND] {
        let mut playback = Playback::new(&events, 1, speed(f64::MAX));
        assert_playback(&mut playback, elapsed, 3, Timestamp::MAX);
    }
}

#[test]
fn a_speed_must_be_finite_and_positive() {
    for text in [
        "NaN", "inf", "+inf", "-inf", "Infinity", "0", "-0", "-1", "", "fast",
    ] {
        assert!(text.parse::<Speed>().is_err(), "{text:?}");
    }
    for text in ["300", "0.5", "1e300", "1.7976931348623157e308"] {
        assert!(text.parse::<Speed>().is_ok(), "{text:?}");
    }
}

#[test]
fn an_offset_is_held_at_the_ends_of_the_timeline() {
    assert_eq!(offset(START, secs(40)), START + secs(40));
    assert_eq!(offset(START, secs(-40)), START - secs(40));
    assert_eq!(offset(START, SignedDuration::MAX), Timestamp::MAX);
    assert_eq!(offset(START, SignedDuration::MIN), Timestamp::MIN);
}

#[test]
fn playing_past_the_last_event_keeps_the_clock_running_from_it() {
    let events = recorded(&[(secs(0), "boot", None), (secs(10), "boot", None)]);
    let mut playback = Playback::new(&events, 1, speed(1.0));
    assert_playback(&mut playback, at(100), 2, events[1].time + secs(90));
    assert_playback(&mut playback, at(200), 2, events[1].time + secs(190));
}

#[test]
fn a_start_past_the_end_or_before_the_first_event_is_held_inside_the_recording() {
    let events = recorded(&[(secs(0), "boot", None), (secs(10), "boot", None)]);
    let mut late = Playback::new(&events, 99, speed(1.0));
    assert_playback(&mut late, at(0), 2, events[1].time);
    let mut early = Playback::new(&events, 0, speed(1.0));
    assert_playback(&mut early, at(0), 1, events[0].time);
}
