use super::{core, topology};
use crate::*;
use std::time::Duration;
use test_case::test_case;

fn spec(regime: Regime, seed: u64, index: usize) -> TrialSpec {
    let workload = pick_workload(regime, index).id();
    let cores = vec![core(0)];
    let cpus = vec![CpuId::from(0)];
    let roles = TrialRole::legacy(regime, workload, seed, &cores, &cpus, &topology()).unwrap();
    TrialSpec {
        id: "t".to_owned(),
        regime,
        workload,
        condition: Condition::Alone,
        cores,
        cpus,
        roles,
        duration: Duration::from_secs(60),
        index,
        seed,
        offsets: PerCore::default(),
        controller: crate::ControllerMode::Managed {
            rotation: 0,
            purpose: crate::Purpose::Isolated,
        },
    }
}

fn first(schedule: LoadSchedule, count: usize) -> Vec<Duration> {
    schedule.periods().take(count).collect()
}

fn duty(pct: u8) -> DutyPct {
    DutyPct::new(pct).unwrap()
}

fn millis(value: u64) -> Duration {
    Duration::from_millis(value)
}

#[test_case(Regime::R1)]
#[test_case(Regime::R2)]
#[test_case(Regime::R5)]
#[test_case(Regime::R6)]
#[test_case(Regime::R7)]
fn regimes_without_load_steps_have_no_schedule(regime: Regime) {
    assert_eq!(LoadSchedule::for_spec(&spec(regime, 1, 0)), None);
}

#[test]
fn load_step_regimes_have_a_schedule() {
    assert_eq!(
        LoadSchedule::for_spec(&spec(Regime::R3, 812, 0)),
        Some(LoadSchedule::Random { seed: 812 })
    );
    assert_eq!(
        LoadSchedule::for_spec(&spec(Regime::R4, 0, 1)),
        Some(LoadSchedule::Duty { pct: duty(50) })
    );
}

#[test]
fn duty_splits_each_hundred_milliseconds() {
    assert_eq!(
        first(LoadSchedule::Duty { pct: duty(25) }, 4),
        [millis(25), millis(75), millis(25), millis(75)]
    );
    assert_eq!(
        first(LoadSchedule::Duty { pct: duty(100) }, 2),
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

#[test_case(LoadSchedule::Duty { pct: duty(25) }, Duration::from_secs(1), toggles(10, 9); "25% over a second")]
#[test_case(LoadSchedule::Duty { pct: duty(50) }, Duration::from_secs(1), toggles(10, 9); "50% over a second")]
#[test_case(LoadSchedule::Duty { pct: duty(25) }, millis(25), toggles(0, 0); "ending at the on period")]
#[test_case(LoadSchedule::Duty { pct: duty(25) }, millis(26), toggles(1, 0); "just past the on period")]
#[test_case(LoadSchedule::Duty { pct: duty(25) }, millis(100), toggles(1, 0); "ending at the run length")]
#[test_case(LoadSchedule::Duty { pct: duty(25) }, millis(101), toggles(1, 1); "just past the period")]
#[test_case(LoadSchedule::Duty { pct: duty(75) }, Duration::ZERO, toggles(0, 0); "zero run")]
fn toggles_do_not_count_a_period_ending_at_the_run_length(
    schedule: LoadSchedule,
    ran: Duration,
    want: Toggles,
) {
    assert_eq!(schedule.toggles(ran), want, "{schedule} over {ran:?}");
}

#[test]
fn random_stops_lead_continues_by_at_most_one() {
    let five = LoadSchedule::Random { seed: 5 };
    for tenth in 0..9000 {
        let Toggles { stops, conts } = five.toggles(millis(tenth * 10));
        assert!(
            stops == conts || stops == conts + 1,
            "{tenth}: {stops} {conts}"
        );
    }
}

#[test]
fn schedules_read_as_text() {
    assert_eq!(
        LoadSchedule::Duty { pct: duty(50) }.to_string(),
        "50% duty, 100 ms period"
    );
    assert_eq!(
        LoadSchedule::Random { seed: 5 }.to_string(),
        "random on/off periods from 10 ms, 50 ms, 200 ms, 1 s, 5 s"
    );
}

#[test_case(LoadSchedule::Random { seed: 5 }, r#"{"kind":"random","seed":5}"#; "random")]
#[test_case(LoadSchedule::Duty { pct: duty(40) }, r#"{"kind":"duty","pct":40}"#; "duty")]
fn schedules_round_trip_as_tagged_objects(schedule: LoadSchedule, json: &str) {
    assert_eq!(serde_json::to_string(&schedule).unwrap(), json);
    assert_eq!(
        serde_json::from_str::<LoadSchedule>(json).unwrap(),
        schedule
    );
}

#[test]
fn duty_above_one_hundred_is_unrepresentable() {
    assert_eq!(DutyPct::new(100).map(DutyPct::get), Some(100));
    assert_eq!(DutyPct::new(101), None);
    assert!(serde_json::from_str::<LoadSchedule>(r#"{"kind":"duty","pct":101}"#).is_err());
    assert!(serde_json::from_str::<LoadSchedule>(r#"{"kind":"duty","pct":200}"#).is_err());
}
