use std::time::Duration;

use test_case::test_case;
use togi_machine::{
    CancellationToken, CoreId, Machine, Partial, PerCore, Reporter, Running, Sample, SampleError,
    Signal, Started, Stopped, Swept, TargetOffset, TrialConditions, TrialFailure, TrialResult,
    TrialSpec, Trials,
};
use togi_sim::day_night::{Scenario, TrialClass, TruthState};

use super::machine::Runtime;
use super::{
    CalendarConfig, Comparator, DayClassification, DayObservation, EndAnswer, FixedMargin, Night,
    Policy, RunError, run, run_with_policy,
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
    #[expect(
        clippy::disallowed_methods,
        reason = "the test policy sets only the in-memory simulated machine"
    )]
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
        night_sleep: Duration::from_hours(1),
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
    #[expect(
        clippy::disallowed_methods,
        reason = "the test policy sets only the in-memory simulated machine"
    )]
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
    #[expect(
        clippy::disallowed_methods,
        reason = "the test probe sets and restores only the in-memory simulated machine"
    )]
    fn night(&mut self, night: &Night, machine: &mut Machine<'_>) -> Result<(), RunError> {
        let count = machine.host.topology()?.len();
        let mut spec = night
            .trials
            .iter()
            .find(|spec| spec.cores.len() == count && spec.regime == togi_machine::Regime::R7)
            .cloned()
            .ok_or(RunError::Calendar("test night menu has no full class"))?;
        spec.duration = Duration::from_mins(30);
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
        let _stopped = running.stop()?;
        machine.trials.ended(&spec.id)?;
        machine.smu.set_all_offsets(TargetOffset::ZERO)?;
        Ok(())
    }
}
struct Silent;
impl Reporter for Silent {
    fn progress(&mut self, _detail: &str) {}
    fn sample(&mut self, _sample: Sample) {}
    fn signal(&mut self, _core: CoreId, _signal: Signal, _pid: Option<u32>, _detail: &str) {}
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
    #[expect(
        clippy::disallowed_methods,
        reason = "the test policy sets only the in-memory simulated machine"
    )]
    fn start_day(&mut self, _day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        machine
            .smu
            .set_all_offsets(TargetOffset::clamped(-i64::from(self.depth)))?;
        Ok(())
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the test policy retreats only the in-memory simulated machine"
    )]
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

/// Night trials fail exactly when a loaded core runs deeper than its CCD's edge.
struct EdgeTrials {
    edges: PerCore<u8>,
}
impl Trials for EdgeTrials {
    fn sweep(&mut self, _cancel: &CancellationToken) -> Result<Swept, togi_machine::Error> {
        Ok(Swept {
            detail: String::new(),
            stopped: confirmed(),
        })
    }
    fn start(
        &mut self,
        _cancel: &CancellationToken,
        spec: TrialSpec,
        _ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let failure = spec
            .cores
            .iter()
            .copied()
            .find(|&core| {
                let depth = spec.offsets.get(core).map_or(0, |offset| -offset.get());
                depth > self.edges.get(core).map_or(0, i64::from)
            })
            .map(|core| TrialFailure {
                signal: Signal::ComputationError,
                core,
                pid: None,
            });
        Ok(Box::new(EdgeRun(TrialResult {
            ran: spec.duration,
            failure,
            ..TrialResult::default()
        })))
    }
    fn ended(&mut self, _id: &str) -> Result<(), togi_machine::Error> {
        Ok(())
    }
    fn samples<'a>(
        &'a self,
        _id: &'a str,
    ) -> Box<dyn Iterator<Item = Result<TrialConditions, SampleError>> + 'a> {
        Box::new(std::iter::empty())
    }
}

struct EdgeRun(TrialResult);
impl Running for EdgeRun {
    fn started(&self) -> Started {
        Started::default()
    }
    fn wait(
        &mut self,
        _cancel: &CancellationToken,
        _reporter: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        Ok(self.0.clone())
    }
    fn stop(self: Box<Self>) -> Result<Stopped, togi_machine::Error> {
        Ok(confirmed())
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the in-memory trial fake never starts a workload"
)]
fn confirmed() -> Stopped {
    Stopped::confirmed()
}

/// Calendar machine seams whose night trials expose the given per-CCD edges.
fn edge_machine(edges: [u8; 2]) -> (Machine<'static>, Night) {
    let scenario = Scenario::builtin("no-write-limit-failure", 1).unwrap();
    let runtime = Runtime::new(scenario, 1, 1.0 / 3000.0, None).unwrap();
    let mut machine = runtime.seams();
    let mut limits = PerCore::default();
    for info in machine.host.topology().unwrap() {
        limits.set(info.core, edges[info.ccd.index()]);
    }
    machine.trials = Box::new(EdgeTrials { edges: limits });
    let night = Night {
        day: 0,
        budget: Duration::from_hours(10_000),
        trials: runtime.night_specs().unwrap(),
    };
    (machine, night)
}

/// Each CCD's applied offset; every core of one CCD must carry the same offset.
fn applied(machine: &mut Machine<'_>) -> Vec<i64> {
    let mut offsets: Vec<i64> = Vec::new();
    for info in machine.host.topology().unwrap() {
        let offset = machine.smu.offset(info.core).unwrap().0;
        let ccd = info.ccd.index();
        if ccd == offsets.len() {
            offsets.push(offset);
        }
        assert_eq!(offsets[ccd], offset, "core {:?} on CCD {ccd}", info.core);
    }
    offsets
}

fn credit(policy: &mut FixedMargin, machine: &mut Machine<'_>, active: Duration) {
    let classification = policy
        .observe_day(0, &DayObservation::Completed { active }, machine)
        .unwrap();
    assert_eq!(classification, DayClassification::NotCpuSuspect);
}

/// Edges 30 and 9 with margin 4 give discovered caps 26 and 5.
fn discovered_fixed_margin() -> (FixedMargin, Machine<'static>) {
    let (mut machine, night) = edge_machine([30, 9]);
    let mut policy = FixedMargin::new(4, 8).unwrap();
    policy.night(&night, &mut machine).unwrap();
    assert_eq!(applied(&mut machine), vec![0, 0]);
    (policy, machine)
}

#[test_case(49 * 3600 - 1, &[0, 0]; "one second short of the hold")]
#[test_case(49 * 3600, &[-8, -5]; "hold reached")]
fn fixed_margin_leaves_stock_only_after_fifty_daily_active_hours(
    remaining_seconds: u64,
    expected: &[i64],
) {
    let (mut policy, mut machine) = discovered_fixed_margin();
    credit(&mut policy, &mut machine, Duration::from_hours(1));
    credit(
        &mut policy,
        &mut machine,
        Duration::from_secs(remaining_seconds),
    );
    policy.start_day(1, &mut machine).unwrap();
    assert_eq!(applied(&mut machine), expected);
}

#[test]
fn fixed_margin_steps_at_most_eight_counts_toward_four_inside_the_observed_edge() {
    let (mut policy, mut machine) = discovered_fixed_margin();
    credit(&mut policy, &mut machine, Duration::from_hours(50));
    policy.start_day(1, &mut machine).unwrap();
    assert_eq!(applied(&mut machine), vec![-8, -5]);
    // A promotion spends the hold, so the next day cannot promote again.
    policy.start_day(2, &mut machine).unwrap();
    assert_eq!(applied(&mut machine), vec![-8, -5]);
    let climb: Vec<Vec<i64>> = (3..7)
        .map(|day| {
            credit(&mut policy, &mut machine, Duration::from_hours(50));
            policy.start_day(day, &mut machine).unwrap();
            applied(&mut machine)
        })
        .collect();
    assert_eq!(
        climb,
        vec![vec![-16, -5], vec![-24, -5], vec![-26, -5], vec![-26, -5]]
    );
}

#[test_case(Some(EndAnswer::Cpu), DayClassification::CpuSuspect, &[-7, -4], &[-25, -4]; "cpu answer retreats")]
#[test_case(None, DayClassification::CpuSuspect, &[-7, -4], &[-25, -4]; "missing answer retreats")]
#[test_case(Some(EndAnswer::Other), DayClassification::NotCpuSuspect, &[-8, -5], &[-26, -5]; "other answer keeps")]
fn fixed_margin_retreats_offsets_and_caps_one_count_after_a_cpu_suspect_ending(
    answer: Option<EndAnswer>,
    classification: DayClassification,
    after_ending: &[i64],
    settled: &[i64],
) {
    let (mut policy, mut machine) = discovered_fixed_margin();
    credit(&mut policy, &mut machine, Duration::from_hours(50));
    policy.start_day(1, &mut machine).unwrap();
    assert_eq!(applied(&mut machine), vec![-8, -5]);
    let ending = DayObservation::Ended {
        active: Duration::from_hours(1),
        answer,
    };
    assert_eq!(
        policy.observe_day(1, &ending, &mut machine).unwrap(),
        classification
    );
    assert_eq!(applied(&mut machine), after_ending);
    // Promoting until nothing moves exposes the cap each CCD retained.
    for day in 2..6 {
        credit(&mut policy, &mut machine, Duration::from_hours(50));
        policy.start_day(day, &mut machine).unwrap();
    }
    assert_eq!(applied(&mut machine), settled);
}

/// Applies a per-CCD depth schedule each morning and records the day of every ending it sees.
struct Schedule {
    depths: Box<dyn Fn(u32) -> Vec<u8>>,
    ended_days: Vec<u32>,
}
impl Schedule {
    fn new(depths: impl Fn(u32) -> Vec<u8> + 'static) -> Self {
        Self {
            depths: Box::new(depths),
            ended_days: Vec::new(),
        }
    }
    fn endings(&self, days: impl Fn(u32) -> bool) -> u32 {
        u32::try_from(self.ended_days.iter().filter(|&&day| days(day)).count()).unwrap()
    }
}
impl Policy for Schedule {
    fn firmware_changed(&mut self, _machine: &mut Machine<'_>) -> Result<(), RunError> {
        Ok(())
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the test policy sets only the in-memory simulated machine"
    )]
    fn start_day(&mut self, day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        let depths = (self.depths)(day);
        for info in machine.host.topology()? {
            machine.smu.set_offset(
                info.core,
                TargetOffset::clamped(-i64::from(depths[info.ccd.index()])),
            )?;
        }
        Ok(())
    }
    fn observe_day(
        &mut self,
        day: u32,
        evidence: &DayObservation,
        _machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError> {
        match evidence {
            DayObservation::Completed { .. } => {}
            DayObservation::Ended { .. } => self.ended_days.push(day),
        }
        Ok(evidence.conservative_classification())
    }
    fn night(&mut self, _night: &Night, _machine: &mut Machine<'_>) -> Result<(), RunError> {
        Ok(())
    }
}

#[test]
fn single_ccd_events_are_charged_only_to_that_ccd_and_cut_at_the_first_year() {
    // CCD 1 stays stock and the scenario has no background, so every ending is a CCD 0 event.
    let mut policy = Schedule::new(|_| vec![20, 0]);
    let metrics = run_with_policy(
        "edge-shallow",
        1,
        CalendarConfig {
            days: 4,
            year_days: 2,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    let all = policy.endings(|_| true);
    let first_year = policy.endings(|day| day < 2);
    assert!(policy.endings(|day| day == 1) > 0 && policy.endings(|day| day == 2) > 0);
    assert_eq!(metrics.true_co_crashes, all);
    assert_eq!(metrics.year_one_true_co_crashes, first_year);
    assert_eq!(metrics.daily_co_crashes, vec![all, 0]);
    assert_eq!(metrics.year_one_co_crashes, vec![first_year, 0]);
    assert_eq!(metrics.climb_co_crashes, vec![all, 0]);
    assert_eq!(metrics.background_events, 0);
    assert_eq!(metrics.cpu_suspect_events, all);
}

#[test]
fn joint_events_count_once_but_charge_both_ccds() {
    // At depth 26 the joint source dominates; each CCD's own edge is about eleven counts deeper.
    let mut policy = Schedule::new(|_| vec![26, 26]);
    let metrics = run_with_policy(
        "both-ccd-interaction",
        1,
        CalendarConfig {
            days: 4,
            year_days: 2,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    let all = policy.endings(|_| true);
    let first_year = policy.endings(|day| day < 2);
    assert!(policy.endings(|day| day == 1) > 0 && policy.endings(|day| day == 2) > 0);
    assert_eq!(metrics.true_co_crashes, all);
    assert_eq!(metrics.year_one_true_co_crashes, first_year);
    assert_eq!(metrics.daily_co_crashes, vec![all, all]);
    assert_eq!(metrics.year_one_co_crashes, vec![first_year, first_year]);
    assert_eq!(metrics.climb_co_crashes, vec![all, all]);
    assert_eq!(metrics.background_events, 0);
}

#[test]
fn background_endings_are_not_true_co_crashes() {
    let mut policy = Schedule::new(|_| vec![0, 0]);
    let metrics = run_with_policy(
        "stock-background",
        1,
        CalendarConfig {
            days: 1000,
            active_hours: 24.0,
            night_hours: 0.0,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    let all = policy.endings(|_| true);
    assert!(all > 0);
    assert_eq!(metrics.background_events, all);
    assert_eq!(metrics.cpu_suspect_events, all);
    assert_eq!(metrics.true_co_crashes, 0);
    assert_eq!(metrics.daily_co_crashes, vec![0, 0]);
}

#[test]
fn convergence_records_day_and_depth_and_ends_the_climb() {
    let seed = 1;
    // The oracle reference stays in the test harness; the policy is only handed a schedule.
    let scenario = Scenario::builtin("single-ccd", seed).unwrap();
    let fractions: Vec<f64> = scenario
        .daily_mix()
        .iter()
        .map(|block| block.fraction)
        .collect();
    let cap = u8::try_from(
        -scenario
            .safe_profile_for(0, 1.0 / 3000.0, Duration::from_hours(8), &fractions)
            .unwrap()[0]
            .get(),
    )
    .unwrap();
    let deep = 34;
    let mut policy = Schedule::new(move |day| vec![if day == 2 { cap } else { deep }]);
    let metrics = run_with_policy(
        "single-ccd",
        seed,
        CalendarConfig {
            days: 5,
            year_days: 4,
            ..CalendarConfig::default()
        },
        &mut policy,
    )
    .unwrap();
    for day in [1, 3, 4] {
        assert!(policy.endings(|ended| ended == day) > 0, "day {day}");
    }
    assert_eq!(metrics.convergence_days, vec![Some(3)]);
    assert_eq!(metrics.converged_depth, vec![Some(cap)]);
    assert!(!metrics.censored);
    assert_eq!(metrics.final_depth, vec![deep]);
    assert_eq!(metrics.max_overshoot, vec![deep - cap]);
    assert_eq!(metrics.true_co_crashes, policy.endings(|_| true));
    assert_eq!(metrics.daily_co_crashes, vec![policy.endings(|_| true)]);
    assert_eq!(
        metrics.year_one_true_co_crashes,
        policy.endings(|day| day < 4)
    );
    assert_eq!(
        metrics.year_one_co_crashes,
        vec![policy.endings(|day| day < 4)]
    );
    assert_eq!(
        metrics.climb_co_crashes,
        vec![policy.endings(|day| day < 2)]
    );
}
