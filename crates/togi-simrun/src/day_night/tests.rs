use std::time::Duration;

use test_case::test_case;
use togi_machine::{
    CancellationToken, CoreId, Machine, Reporter, Sample, Signal, TargetOffset, TrialResult,
    TrialSpec,
};
use togi_sim::day_night::{Scenario, TrialClass, TruthState};

use super::{
    CalendarConfig, Comparator, DayClassification, DayObservation, EndAnswer, Night, Policy,
    RunError, run, run_with_policy,
};

#[derive(Default)]
struct Observer {
    observations: Vec<DayObservation>,
    nights: Vec<u32>,
    night_sleep: Duration,
    firmware_changes: usize,
    bios: Vec<String>,
    classification: Option<DayClassification>,
    topology: Vec<togi_machine::CoreInfo>,
}

impl Policy for Observer {
    fn firmware_changed(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.firmware_changes += 1;
        self.bios.push(machine.host.bios_context()?.bios_version);
        Ok(())
    }
    fn start_day(&mut self, _day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        if self.topology.is_empty() {
            self.topology = machine.host.topology()?;
        }
        machine.smu.set_all_offsets(TargetOffset::ZERO)?;
        Ok(())
    }
    fn observe_day(
        &mut self,
        _day: u32,
        evidence: &DayObservation,
        _machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError> {
        self.observations.push(evidence.clone());
        Ok(self
            .classification
            .unwrap_or_else(|| evidence.conservative_classification()))
    }
    fn night(&mut self, night: &Night, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.nights.push(night.day);
        machine
            .clock
            .sleep(&CancellationToken::new(), self.night_sleep)?;
        Ok(())
    }
}

#[test]
fn sleep_never_becomes_daily_or_night_exposure() {
    let mut policy = Observer {
        night_sleep: Duration::from_secs(3600),
        ..Observer::default()
    };
    let metrics = run_with_policy(
        "no-write-limit-failure",
        1,
        CalendarConfig {
            days: 7,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    assert_eq!(policy.nights, vec![0, 2, 4]);
    assert_eq!(metrics.active_hours, 56.0);
    assert_eq!(metrics.night_hours, 0.0);
    assert_eq!(metrics.sleep_hours, 112.0);
    assert_eq!(metrics.true_co_crashes, 0);
    assert_eq!(metrics.daily_co_crashes, vec![0, 0]);
}

#[test_case("non-cpu-correct", Some(EndAnswer::Other), 0; "correct other answer")]
#[test_case("non-cpu-wrong", Some(EndAnswer::Cpu), 1; "incorrect cpu answer")]
#[test_case("non-cpu-missing", None, 1; "missing answer")]
fn non_cpu_endings_keep_answers_separate_from_true_cause(
    scenario: &str,
    answer: Option<EndAnswer>,
    suspects: u32,
) {
    let mut policy = Observer::default();
    let metrics = run_with_policy(
        scenario,
        2,
        CalendarConfig {
            days: 31,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    let answers: Vec<Option<EndAnswer>> = policy
        .observations
        .iter()
        .filter_map(|observation| match observation {
            DayObservation::Completed { .. } => None,
            DayObservation::Ended { answer, .. } => Some(*answer),
        })
        .collect();
    assert_eq!(answers, vec![answer]);
    assert_eq!(metrics.non_cpu_events, 1);
    assert_eq!(metrics.true_co_crashes, 0);
    assert_eq!(metrics.year_one_true_co_crashes, 0);
    assert_eq!(metrics.cpu_suspect_events, suspects);
    assert_eq!(metrics.active_hours, 248.0);
    assert_eq!(metrics.sleep_hours, 496.0);
}

#[test]
fn graphics_and_power_endings_both_use_the_answer_interface() {
    let mut policy = Observer::default();
    let metrics = run_with_policy(
        "non-cpu-correct",
        2,
        CalendarConfig {
            days: 121,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    let answers: Vec<Option<EndAnswer>> = policy
        .observations
        .iter()
        .filter_map(|observation| match observation {
            DayObservation::Completed { .. } => None,
            DayObservation::Ended { answer, .. } => Some(*answer),
        })
        .collect();
    assert_eq!(
        answers,
        vec![Some(EndAnswer::Other), Some(EndAnswer::Other)]
    );
    assert_eq!(metrics.non_cpu_events, 2);
    assert_eq!(metrics.cpu_suspect_events, 0);
}

#[test]
fn night_work_cannot_pay_the_fixed_comparators_daily_hold() {
    let metrics = run(
        "no-write-limit-failure",
        1,
        Comparator::FixedMargin,
        CalendarConfig {
            days: 10,
            active_hours: 1.0,
            night_hours: 16.0,
            night_weekdays: vec![0, 1, 2, 3, 4, 5, 6],
            ..CalendarConfig::default()
        },
    )
    .unwrap();
    assert_eq!(metrics.active_hours, 10.0);
    assert!(metrics.night_hours > 50.0);
    assert_eq!(metrics.promotions, vec![0, 0]);
    assert_eq!(metrics.final_depth, vec![0, 0]);
    assert_eq!(
        metrics.active_hours + metrics.night_hours + metrics.sleep_hours,
        240.0
    );
}

#[test]
fn stock_nonconvergence_is_retained_as_right_censoring() {
    let metrics = run(
        "no-write-limit-failure",
        1,
        Comparator::CoZero,
        CalendarConfig {
            days: 1,
            ..CalendarConfig::default()
        },
    )
    .unwrap();
    assert_eq!(metrics.convergence_days, vec![None, None]);
    assert_eq!(metrics.converged_depth, vec![None, None]);
    assert_eq!(metrics.final_depth, vec![0, 0]);
    assert!(metrics.censored);
}

#[test]
fn configured_firmware_transition_reaches_machine_and_policy() {
    let mut policy = Observer::default();
    let metrics = run_with_policy(
        "firmware-change",
        1,
        CalendarConfig {
            days: 3,
            firmware_change_day: Some(1),
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    assert_eq!(policy.firmware_changes, 1);
    assert_eq!(policy.bios.len(), 1);
    assert!(policy.bios[0].ends_with("-transition"));
    assert!(!metrics.fixed_cap);
}

#[derive(Default)]
struct NightProbe {
    spec: Option<TrialSpec>,
    result: Option<TrialResult>,
}
impl Policy for NightProbe {
    fn firmware_changed(&mut self, _machine: &mut Machine<'_>) -> Result<(), RunError> {
        Ok(())
    }
    fn start_day(&mut self, _day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        machine.smu.set_all_offsets(TargetOffset::ZERO)?;
        Ok(())
    }
    fn observe_day(
        &mut self,
        _day: u32,
        _evidence: &DayObservation,
        _machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError> {
        Ok(DayClassification::NotCpuSuspect)
    }
    fn night(&mut self, night: &Night, machine: &mut Machine<'_>) -> Result<(), RunError> {
        let count = machine.host.topology()?.len();
        let mut spec = night
            .trials
            .iter()
            .find(|spec| spec.cores.len() == count && spec.regime == togi_machine::Regime::R7)
            .cloned()
            .ok_or(RunError::Calendar("test night menu has no full class"))?;
        spec.duration = Duration::from_secs(1800);
        let offset = TargetOffset::new(-50).map_err(|_| RunError::Calendar("test offset"))?;
        machine.smu.set_all_offsets(offset)?;
        for info in machine.host.topology()? {
            spec.offsets.set(info.core, offset);
        }
        self.spec = Some(spec.clone());
        let cancel = CancellationToken::new();
        let stopped = machine.trials.sweep(&cancel)?.stopped;
        let mut running = machine
            .trials
            .start(&cancel, spec.clone(), stopped)
            .map_err(|partial| partial.error)?;
        self.result = Some(
            running
                .wait(&cancel, &mut Silent)
                .map_err(|partial| partial.error)?,
        );
        running.stop()?;
        machine.trials.ended(&spec.id)?;
        machine.smu.set_all_offsets(TargetOffset::ZERO)?;
        Ok(())
    }
}
struct Silent;
impl Reporter for Silent {
    fn progress(&mut self, _detail: &str) {}
    fn sample(&mut self, _sample: Sample) {}
    fn signal(&mut self, _core: CoreId, _signal: Signal, _detail: &str) {}
}

#[test]
fn night_trials_use_the_same_class_history_and_draw_as_daily_truth() {
    let seed = 17;
    let mut policy = NightProbe::default();
    let metrics = run_with_policy(
        "slope-steep",
        seed,
        CalendarConfig {
            days: 1,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    // Oracle expectations remain in the test harness, outside the policy's machine-only interface.
    let scenario = Scenario::builtin("slope-steep", seed).unwrap();
    let mut state = TruthState::cold(scenario.ccd_count()).unwrap();
    for block in scenario.daily_mix() {
        state
            .advance(
                Some(&block.class),
                Duration::from_secs_f64(8.0 * block.fraction * 3600.0),
            )
            .unwrap();
    }
    let spec = policy.spec.as_ref().unwrap();
    let class = TrialClass::from_spec(spec).unwrap();
    let offsets = vec![TargetOffset::new(-50).unwrap(); scenario.ccd_count()];
    let expected = scenario
        .draw(&class, &offsets, &state, spec.duration, 0, seed, 4)
        .unwrap()
        .unwrap();
    let actual = policy.result.as_ref().unwrap();
    assert_eq!(actual.ran, expected.elapsed);
    assert!(actual.failure.is_some());
    assert_eq!(metrics.night_hours, expected.elapsed.as_secs_f64() / 3600.0);
    assert_eq!(metrics.true_co_crashes, 0);
    assert_eq!(metrics.cpu_suspect_events, 0);
}

#[test]
fn cpu_suspect_metrics_record_the_policy_classification_not_a_runner_rule() {
    let mut policy = Observer {
        classification: Some(DayClassification::NotCpuSuspect),
        ..Observer::default()
    };
    let metrics = run_with_policy(
        "non-cpu-wrong",
        2,
        CalendarConfig {
            days: 31,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    assert_eq!(metrics.non_cpu_events, 1);
    assert_eq!(metrics.cpu_suspect_events, 0);
    assert!(policy.observations.iter().any(|observation| matches!(
        observation,
        DayObservation::Ended {
            answer: Some(EndAnswer::Cpu),
            ..
        }
    )));
}

#[test]
fn single_ccd_fixture_presents_one_physical_ccd_through_machine_seams() {
    let mut policy = Observer::default();
    let metrics = run_with_policy(
        "single-ccd",
        1,
        CalendarConfig {
            days: 1,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    assert_eq!(policy.topology.len(), 8);
    assert!(policy.topology.iter().all(|info| info.ccd.get() == 0));
    assert_eq!(metrics.daily_co_crashes, vec![0]);
    assert_eq!(metrics.final_depth, vec![0]);
}

struct SettingPolicy {
    depth: u8,
    retreat_on_completion: bool,
}
impl Policy for SettingPolicy {
    fn firmware_changed(&mut self, _machine: &mut Machine<'_>) -> Result<(), RunError> {
        Ok(())
    }
    fn start_day(&mut self, _day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        machine
            .smu
            .set_all_offsets(TargetOffset::clamped(-i64::from(self.depth)))?;
        Ok(())
    }
    fn observe_day(
        &mut self,
        _day: u32,
        evidence: &DayObservation,
        machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError> {
        if self.retreat_on_completion && matches!(evidence, DayObservation::Completed { .. }) {
            machine.smu.set_all_offsets(TargetOffset::ZERO)?;
        }
        Ok(DayClassification::NotCpuSuspect)
    }
    fn night(&mut self, _night: &Night, _machine: &mut Machine<'_>) -> Result<(), RunError> {
        Ok(())
    }
}

#[test]
fn retreat_metrics_count_lost_depth_not_incidents() {
    let mut policy = SettingPolicy {
        depth: 8,
        retreat_on_completion: true,
    };
    let metrics = run_with_policy(
        "no-write-limit-failure",
        1,
        CalendarConfig {
            days: 1,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    assert_eq!(metrics.unattributed_retreat_counts, vec![8, 8]);
    assert_eq!(metrics.justified_retreat_counts, vec![0, 0]);
    assert_eq!(metrics.background_retreat_counts, vec![0, 0]);
    assert_eq!(metrics.promotions, vec![1, 1]);
}

#[test]
fn dangerously_overshot_profiles_are_not_converged() {
    let mut policy = SettingPolicy {
        depth: 10,
        retreat_on_completion: false,
    };
    let metrics = run_with_policy(
        "edge-shallow",
        1,
        CalendarConfig {
            days: 1,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    assert_eq!(metrics.convergence_days, vec![None, None]);
    assert!(metrics.max_overshoot.iter().all(|overshoot| *overshoot > 1));
    assert!(metrics.censored);
}
