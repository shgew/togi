use crate::*;
use std::time::Duration;

fn spec(regime: Regime, seed: u64, index: usize) -> TrialSpec {
    TrialSpec {
        id: "t".to_owned(),
        regime,
        workload: pick_workload(regime, index).clone(),
        condition: Condition::Alone,
        cores: Vec::new(),
        cpus: Vec::new(),
        duration: Duration::from_secs(60),
        index,
        seed,
    }
}

fn first(schedule: LoadSchedule, count: usize) -> Vec<Duration> {
    schedule.periods().take(count).collect()
}

fn millis(value: u64) -> Duration {
    Duration::from_millis(value)
}

#[test]
fn only_load_step_regimes_have_a_schedule() {
    for regime in [Regime::R1, Regime::R2, Regime::R5, Regime::R6, Regime::R7] {
        assert_eq!(LoadSchedule::for_spec(&spec(regime, 1, 0)), None);
    }
    assert_eq!(
        LoadSchedule::for_spec(&spec(Regime::R3, 812, 0)),
        Some(LoadSchedule::Random { seed: 812 })
    );
    assert_eq!(
        LoadSchedule::for_spec(&spec(Regime::R4, 0, 1)),
        Some(LoadSchedule::Duty { pct: 50 })
    );
}

#[test]
fn duty_splits_each_hundred_milliseconds() {
    assert_eq!(
        first(LoadSchedule::Duty { pct: 25 }, 4),
        [millis(25), millis(75), millis(25), millis(75)]
    );
    assert_eq!(
        first(LoadSchedule::Duty { pct: 100 }, 2),
        [millis(100), Duration::ZERO]
    );
}

#[test]
fn random_periods_repeat_per_seed_and_come_from_the_set() {
    let five = LoadSchedule::Random { seed: 5 };
    let stream = first(five, 100);
    assert_eq!(stream, first(five, 100));
    assert_ne!(stream, first(LoadSchedule::Random { seed: 6 }, 100));
    let allowed = [
        millis(10),
        millis(50),
        millis(200),
        Duration::from_secs(1),
        Duration::from_secs(5),
    ];
    assert!(stream.iter().all(|period| allowed.contains(period)));
}

fn toggles(stops: u32, conts: u32) -> Toggles {
    Toggles { stops, conts }
}

#[test]
fn toggles_do_not_count_a_period_ending_at_the_run_length() {
    let duty = |pct| LoadSchedule::Duty { pct };
    for (schedule, ran, want) in [
        (duty(25), Duration::from_secs(1), toggles(10, 9)),
        (duty(50), Duration::from_secs(1), toggles(10, 9)),
        (duty(25), millis(25), toggles(0, 0)),
        (duty(25), millis(26), toggles(1, 0)),
        (duty(25), millis(100), toggles(1, 0)),
        (duty(25), millis(101), toggles(1, 1)),
        (duty(75), Duration::ZERO, toggles(0, 0)),
    ] {
        assert_eq!(schedule.toggles(ran), want, "{schedule} over {ran:?}");
    }
}

#[test]
fn random_stops_lead_continues_by_at_most_one() {
    let five = LoadSchedule::Random { seed: 5 };
    for tenth in 0..9000 {
        let Toggles { stops, conts } = five.toggles(millis(tenth * 10));
        assert!(stops == conts || stops == conts + 1, "{tenth}: {stops} {conts}");
    }
}

#[test]
fn schedules_read_as_text() {
    assert_eq!(
        LoadSchedule::Duty { pct: 50 }.to_string(),
        "50% duty, 100ms period"
    );
    assert_eq!(
        LoadSchedule::Random { seed: 5 }.to_string(),
        "random on/off periods from 10ms, 50ms, 200ms, 1s, 5s"
    );
}
