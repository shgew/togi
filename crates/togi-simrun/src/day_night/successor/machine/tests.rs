use test_case::test_case;

use super::*;

const SEED: u64 = 17;

fn scenario(id: &str) -> Scenario {
    Scenario::builtin(id, SEED).unwrap()
}

fn full_class(scenario: &Scenario) -> TrialClass {
    scenario
        .daily_mix()
        .iter()
        .max_by_key(|block| block.class.loaded().len())
        .unwrap()
        .class
        .clone()
}

fn profile(scenario: &Scenario, depths: PerCore<u8>) -> Profile {
    Profile::new(
        depths
            .iter()
            .map(|(core, value)| (core, Depth::new(value).unwrap()))
            .collect(),
        scenario.topology(),
    )
    .unwrap()
}

#[expect(
    clippy::disallowed_methods,
    reason = "tests write only simulator registers"
)]
fn set_core(machine: &mut Machine<'_>, core: CoreId, depth: u8) {
    machine
        .smu
        .set_offset(core, Depth::new(depth).unwrap().offset().unwrap())
        .unwrap();
}

#[expect(
    clippy::disallowed_methods,
    reason = "tests write only simulator registers"
)]
fn set_all(machine: &mut Machine<'_>, depth: u8) {
    machine
        .smu
        .set_all_offsets(Depth::new(depth).unwrap().offset().unwrap())
        .unwrap();
}

#[test]
fn nonuniform_readbacks_keep_every_physical_core_and_each_write_boundary() {
    let scenario = scenario("successor-within-ccd-heterogeneous");
    let mut runtime = Runtime::new(scenario.clone(), SEED).unwrap();
    let mut machine = runtime.seams();
    let mut expected = PerCore::default();
    for info in scenario.topology() {
        let depth = info.core.get() + 1;
        set_core(&mut machine, info.core, depth);
        expected.set(info.core, depth);
    }
    assert_eq!(runtime.depths().unwrap(), expected);
    let observations = runtime.drain_observations();
    assert_eq!(observations.len(), scenario.topology().len());
    assert_eq!(observations.last().unwrap().profile, expected);
    for observation in observations {
        assert_eq!(observation.before, Duration::ZERO);
        assert_eq!(observation.after, Duration::ZERO);
        assert!(!observation.night);
        assert!(matches!(observation.kind, ObservationKind::ProfileChange));
        assert_eq!(observation.profile.len(), scenario.topology().len());
    }
    assert_eq!(runtime.drain_observations().len(), 0);
}

#[test]
fn actual_failed_prefix_uses_the_full_nonuniform_profile_for_draw_and_integrals() {
    let scenario = scenario("successor-within-ccd-heterogeneous");
    let class = full_class(&scenario);
    let mut runtime = Runtime::new(scenario.clone(), SEED).unwrap();
    let mut machine = runtime.seams();
    // Cores on the same CCD deliberately have different depths: no worst-core collapse.
    for info in scenario.topology() {
        set_core(
            &mut machine,
            info.core,
            if info.core.get() % 2 == 0 { 50 } else { 0 },
        );
    }
    let depths = runtime.depths().unwrap();
    let profile = profile(&scenario, depths);
    let history = TruthState::cold(scenario.topology()).unwrap();
    let intended = class.duration();
    let expected = scenario
        .draw(&class, &profile, &history, intended, 1, SEED, 0)
        .unwrap()
        .unwrap();
    runtime.drain_observations();
    let event = runtime.active(&class, intended).unwrap();
    assert_eq!(event.elapsed, expected.elapsed);
    assert_eq!(event.cause, Some(Cause::Truth(expected.cause)));
    assert_eq!(event.answer, Some(EndAnswer::Cpu));
    assert!(event.elapsed < intended);
    let observation = runtime.drain_observations().pop().unwrap();
    assert_eq!(observation.before, Duration::ZERO);
    assert_eq!(observation.after, expected.elapsed);
    assert_eq!(observation.profile, depths);
    let ObservationKind::Active {
        seconds,
        exposure,
        cause,
        launch,
        intended: requested,
        ..
    } = observation.kind
    else {
        panic!("expected active truth observation");
    };
    assert_eq!(requested, intended);
    assert_eq!(
        seconds,
        scenario
            .slice_seconds(
                &class,
                &profile,
                &history,
                expected.elapsed,
                Duration::from_secs(u64::from(class.regime().warm_up_s())),
            )
            .unwrap()
    );
    assert_eq!(
        exposure,
        scenario
            .evaluate(&class, &profile, &history, expected.elapsed, 1)
            .unwrap()
    );
    assert_eq!(cause, Some(Cause::Truth(expected.cause)));
    assert!(launch);
    for (core, depth) in depths.iter() {
        if depth == 0 {
            assert_eq!(exposure.co_per_core.get(core), Some(0.0));
        }
    }
}

#[test]
fn long_daily_runs_checkpoint_without_restarting_onset_or_recounting_launches() {
    let scenario = scenario("successor-weak-core-delayed");
    let class = full_class(&scenario)
        .with_duration(Duration::from_mins(10))
        .unwrap();
    let mut runtime = Runtime::new(scenario.clone(), SEED).unwrap();
    let event = runtime.active(&class, Duration::from_mins(25)).unwrap();
    assert_eq!(event.cause, None);
    assert_eq!(event.elapsed, Duration::from_mins(25));
    let observations = runtime.drain_observations();
    assert_eq!(observations.len(), 3);
    let profile = Profile::uniform(scenario.topology(), Depth::default());
    let mut history = TruthState::cold(scenario.topology()).unwrap();
    let mut observed = Duration::ZERO;
    let mut intended = Duration::ZERO;
    for (index, observation) in observations.into_iter().enumerate() {
        let ObservationKind::Active {
            seconds,
            exposure,
            launch,
            intended: requested,
            ..
        } = observation.kind
        else {
            panic!("expected active truth observation");
        };
        assert_eq!(launch, index == 0);
        assert_eq!(observation.before, observed);
        assert_eq!(seconds.intended, class.duration());
        assert_eq!(
            seconds,
            scenario
                .slice_seconds(
                    &class,
                    &profile,
                    &history,
                    seconds.observed,
                    Duration::from_secs(u64::from(class.regime().warm_up_s())),
                )
                .unwrap()
        );
        assert_eq!(
            exposure,
            scenario
                .evaluate(&class, &profile, &history, seconds.observed, 1)
                .unwrap()
        );
        history
            .advance_profile(Some(&class), &profile, seconds.observed)
            .unwrap();
        observed += seconds.observed;
        if launch {
            intended += requested;
        }
        assert_eq!(observation.after, observed);
    }
    assert_eq!(observed, Duration::from_mins(25));
    assert_eq!(intended, Duration::from_mins(25));
}

#[test]
fn transient_profile_changes_reset_onset_even_when_the_original_profile_is_restored() {
    let scenario = scenario("successor-weak-core-delayed");
    let class = full_class(&scenario)
        .with_duration(Duration::from_mins(10))
        .unwrap();
    let mut runtime = Runtime::new(scenario.clone(), SEED).unwrap();
    let mut machine = runtime.seams();
    runtime.active(&class, Duration::from_mins(10)).unwrap();
    runtime.drain_observations();
    set_core(&mut machine, scenario.topology()[0].core, 1);
    set_core(&mut machine, scenario.topology()[0].core, 0);
    runtime.active(&class, Duration::from_mins(10)).unwrap();
    let observations = runtime.drain_observations();
    assert_eq!(observations.len(), 3);
    assert!(matches!(
        observations[0].kind,
        ObservationKind::ProfileChange
    ));
    assert!(matches!(
        observations[1].kind,
        ObservationKind::ProfileChange
    ));
    let ObservationKind::Active { seconds, .. } = &observations[2].kind else {
        panic!("expected active truth observation");
    };
    let stock = Profile::uniform(scenario.topology(), Depth::default());
    let history = TruthState::cold(scenario.topology()).unwrap();
    assert_eq!(
        *seconds,
        scenario
            .slice_seconds(
                &class,
                &stock,
                &history,
                Duration::from_mins(10),
                Duration::from_secs(u64::from(class.regime().warm_up_s())),
            )
            .unwrap()
    );
}

struct Silent;
impl Reporter for Silent {
    fn progress(&mut self, _detail: &str) {}
    fn sample(&mut self, _sample: togi_machine::Sample) {}
    fn signal(&mut self, _core: CoreId, _signal: Signal, _pid: Option<u32>, _detail: &str) {}
}

#[test]
fn night_prefix_and_sleep_account_actual_time_and_firmware_resets_all_cores() {
    let scenario = scenario("successor-core-interactions");
    let class = full_class(&scenario)
        .with_duration(Duration::from_mins(20))
        .unwrap();
    let mut runtime = Runtime::new(scenario.clone(), SEED).unwrap();
    let mut machine = runtime.seams();
    set_all(&mut machine, 25);
    let depths = runtime.depths().unwrap();
    let profile = profile(&scenario, depths);
    runtime.drain_observations();
    runtime.begin_night(Duration::from_mins(5));
    let cancel = CancellationToken::new();
    let spec = class
        .to_spec("one-night-trial".into(), PerCore::default(), 0, SEED)
        .unwrap();
    let ready = machine.trials.sweep(&cancel).unwrap().stopped;
    let mut running = machine
        .trials
        .start(&cancel, spec, ready)
        .map_err(|partial| partial.error)
        .unwrap();
    let result = running.wait(&cancel, &mut Silent);
    let ran = match result {
        Ok(result) => result.ran,
        Err(partial) => {
            assert!(matches!(partial.error, Error::Cancelled));
            partial.value.ran
        }
    };
    let _stopped = running.stop().unwrap();
    assert_eq!(runtime.end_night(), ran);
    let observations = runtime.drain_observations();
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    assert!(observation.night);
    assert_eq!(
        observation.after.checked_sub(observation.before).unwrap(),
        ran
    );
    let ObservationKind::Active {
        exposure,
        trial_id,
        cause,
        intended,
        ..
    } = &observation.kind
    else {
        panic!("expected active truth observation");
    };
    assert_eq!(trial_id.as_deref(), Some("one-night-trial"));
    assert_eq!(*intended, class.duration());
    let mut history = TruthState::cold(scenario.topology()).unwrap();
    assert_eq!(
        *exposure,
        scenario
            .evaluate(&class, &profile, &history, ran, 1)
            .unwrap()
    );
    history
        .advance_profile(Some(&class), &profile, ran)
        .unwrap();
    // Either the truth failure or the operator-ended allocation interrupts the run.
    history.interrupt();
    assert_eq!(cause.is_some(), ran < Duration::from_mins(5));
    runtime.sleep(Duration::from_mins(30)).unwrap();
    history
        .advance_profile(None, &profile, Duration::from_mins(30))
        .unwrap();
    let sleep = runtime.drain_observations().pop().unwrap();
    assert!(matches!(sleep.kind, ObservationKind::Sleep));
    assert!(!sleep.night);
    assert_eq!(sleep.before, ran);
    assert_eq!(sleep.after, ran + Duration::from_mins(30));
    assert_eq!(sleep.profile, depths);
    let event = runtime.active(&class, Duration::from_mins(1)).unwrap();
    let resumed = runtime.drain_observations().pop().unwrap();
    let ObservationKind::Active { exposure, .. } = resumed.kind else {
        panic!("expected resumed active observation");
    };
    assert_eq!(resumed.before, sleep.after);
    assert_eq!(
        resumed.after.checked_sub(resumed.before).unwrap(),
        event.elapsed
    );
    assert_eq!(
        exposure,
        scenario
            .evaluate(&class, &profile, &history, event.elapsed, 1)
            .unwrap(),
    );
    runtime.firmware_changed().unwrap();
    let stock: PerCore<u8> = scenario
        .topology()
        .iter()
        .map(|info| (info.core, 0))
        .collect();
    assert_eq!(runtime.depths().unwrap(), stock);
    let reset = runtime.drain_observations().pop().unwrap();
    assert_eq!(reset.profile, stock);
    assert_eq!(reset.before, reset.after);
    assert!(matches!(reset.kind, ObservationKind::ProfileChange));
}

/// Start, wait for and stop one separate night trial launch; the observed active time.
fn launch(machine: &mut Machine<'_>, class: &TrialClass, index: usize) -> Duration {
    let cancel = CancellationToken::new();
    let spec = class
        .to_spec(format!("launch-{index}"), PerCore::default(), index, SEED)
        .unwrap();
    let ready = machine.trials.sweep(&cancel).unwrap().stopped;
    let mut running = machine
        .trials
        .start(&cancel, spec, ready)
        .map_err(|partial| partial.error)
        .unwrap();
    let ran = match running.wait(&cancel, &mut Silent) {
        Ok(result) => result.ran,
        Err(partial) => partial.value.ran,
    };
    let _ = running.stop().unwrap();
    ran
}

fn warm_up(class: &TrialClass) -> Duration {
    Duration::from_secs(u64::from(class.regime().warm_up_s()))
}

/// At depth 25 only the weak cores of the delayed scenario carry a CO hazard, and it waits
/// for 600 uninterrupted seconds.
#[test]
fn separate_night_launches_never_accumulate_the_delayed_onset() {
    let scenario = scenario("successor-weak-core-delayed");
    let class = full_class(&scenario)
        .with_duration(Duration::from_mins(5))
        .unwrap();
    let mut runtime = Runtime::new(scenario.clone(), SEED).unwrap();
    let mut machine = runtime.seams();
    set_all(&mut machine, 25);
    let profile = profile(&scenario, runtime.depths().unwrap());
    runtime.drain_observations();
    runtime.begin_night(Duration::from_hours(2));
    // Three separate 5-minute starts are 15 minutes of exposure in total but no start is
    // ever 600 seconds into its own clock.
    for index in 0..3 {
        launch(&mut machine, &class, index);
    }
    runtime.end_night();
    let observations = runtime.drain_observations();
    assert_eq!(observations.len(), 3);
    let cold = TruthState::cold(scenario.topology()).unwrap();
    for (index, observation) in observations.iter().enumerate() {
        let ObservationKind::Active {
            seconds,
            exposure,
            launch,
            ..
        } = &observation.kind
        else {
            panic!("expected active truth observation");
        };
        assert!(*launch, "launch {index}");
        assert_eq!(
            *seconds,
            scenario
                .slice_seconds(&class, &profile, &cold, seconds.observed, warm_up(&class))
                .unwrap(),
            "launch {index}"
        );
        assert_eq!(
            *exposure,
            scenario
                .evaluate(&class, &profile, &cold, seconds.observed, 1)
                .unwrap(),
            "launch {index}"
        );
        assert!(
            exposure.co_per_core.iter().all(|(_, count)| count <= 0.0),
            "launch {index} reached the delayed onset"
        );
    }
    // Heat is memory, not part of the uninterrupted clock: it survives the interruptions.
    let heat = runtime
        .0
        .borrow()
        .truth
        .heat_seconds(scenario.topology()[0].ccd)
        .unwrap();
    assert!(heat > 0.0);
}

#[test]
fn a_checkpointed_launch_reaches_the_delayed_onset_that_separate_launches_cannot() {
    let scenario = scenario("successor-weak-core-delayed");
    let class = full_class(&scenario)
        .with_duration(Duration::from_mins(5))
        .unwrap();
    let mut runtime = Runtime::new(scenario.clone(), SEED).unwrap();
    let mut machine = runtime.seams();
    set_all(&mut machine, 25);
    runtime.drain_observations();
    // One 15-minute launch is checkpointed in 5-minute slices on one clock.
    runtime.active(&class, Duration::from_mins(15)).unwrap();
    let observations = runtime.drain_observations();
    assert!(observations.len() >= 3);
    let mut late = 0.0;
    for (index, observation) in observations.iter().enumerate() {
        let ObservationKind::Active {
            exposure, launch, ..
        } = &observation.kind
        else {
            panic!("expected active truth observation");
        };
        assert_eq!(*launch, index == 0);
        let total: f64 = exposure.co_per_core.iter().map(|(_, count)| count).sum();
        if index < 2 {
            assert!(total <= 0.0, "slice {index} precedes the 600 s onset");
        } else {
            late += total;
        }
    }
    assert!(late > 0.0);
}

/// A seed and uniform depth whose first draw on a registered non-CPU day has the wanted
/// shape against the interruption's half-run point.
struct Fixture {
    seed: u64,
    depth: u8,
}

fn find_fixture(
    id: &str,
    day: u32,
    depths: &[u8],
    wanted: impl Fn(Option<Failure>, Duration) -> bool,
) -> Fixture {
    for &depth in depths {
        for seed in 0..200 {
            let scenario = Scenario::builtin(id, seed).unwrap();
            let class = full_class(&scenario);
            let duration = class.duration();
            let profile = Profile::uniform(scenario.topology(), Depth::new(depth).unwrap());
            let history = TruthState::cold(scenario.topology()).unwrap();
            let failure = scenario
                .draw(&class, &profile, &history, duration, day, seed, 0)
                .unwrap();
            if wanted(failure, duration / 2) {
                return Fixture { seed, depth };
            }
        }
    }
    panic!("no deterministic fixture for {id} on day {day}");
}

fn day_runtime(id: &str, fixture: &Fixture, day: u32) -> (Runtime, TrialClass) {
    let scenario = Scenario::builtin(id, fixture.seed).unwrap();
    let class = full_class(&scenario);
    let mut runtime = Runtime::new(scenario, fixture.seed).unwrap();
    if fixture.depth > 0 {
        set_all(&mut runtime.seams(), fixture.depth);
    }
    runtime.drain_observations();
    runtime.set_day(day);
    (runtime, class)
}

const DEEP: [u8; 7] = [50, 45, 40, 35, 30, 25, 20];

#[test_case("successor-non-cpu-correct", 31, Cause::Graphics, Some(EndAnswer::Other), false; "graphics answered not CPU")]
#[test_case("successor-non-cpu-wrong", 31, Cause::Graphics, Some(EndAnswer::Cpu), false; "graphics answered CPU")]
#[test_case("successor-non-cpu-missing", 31, Cause::Graphics, None, false; "graphics unanswered")]
#[test_case("successor-non-cpu-correct", 121, Cause::Power, Some(EndAnswer::Other), false; "power answered not CPU")]
#[test_case("successor-non-cpu-wrong", 121, Cause::Power, Some(EndAnswer::Cpu), false; "power answered CPU")]
#[test_case("successor-non-cpu-missing", 121, Cause::Power, None, false; "power unanswered")]
#[test_case("successor-non-cpu-correct", 31, Cause::Graphics, Some(EndAnswer::Other), true; "graphics before a later truth failure")]
#[test_case("successor-non-cpu-wrong", 121, Cause::Power, Some(EndAnswer::Cpu), true; "power before a later truth failure")]
#[test_case("successor-non-cpu-missing", 31, Cause::Graphics, None, true; "unanswered before a later truth failure")]
fn non_cpu_day_reports_the_registered_answer_beside_its_metric_cause(
    id: &str,
    day: u32,
    cause: Cause,
    answer: Option<EndAnswer>,
    later_truth_failure: bool,
) {
    let fixture = if later_truth_failure {
        find_fixture(id, day, &DEEP, |failure, half| {
            failure.is_some_and(|failure| failure.elapsed > half)
        })
    } else {
        find_fixture(id, day, &[0], |failure, half| {
            failure.is_none_or(|failure| failure.elapsed > half)
        })
    };
    let (mut runtime, class) = day_runtime(id, &fixture, day);
    let duration = class.duration();
    let event = runtime.active(&class, duration).unwrap();
    assert_eq!(event.elapsed, duration / 2);
    assert_eq!(event.cause, Some(cause));
    assert_eq!(event.answer, answer);
    assert!(runtime.0.borrow().non_cpu_today);
    let observation = runtime.drain_observations().pop().unwrap();
    assert_eq!(observation.after - observation.before, duration / 2);
    let ObservationKind::Active {
        cause: ledger,
        intended,
        ..
    } = observation.kind
    else {
        panic!("expected active observation");
    };
    assert_eq!(ledger, Some(cause));
    assert_eq!(intended, duration);
}

#[test_case("successor-non-cpu-correct", 31; "graphics")]
#[test_case("successor-non-cpu-wrong", 121; "power")]
fn a_non_cpu_interruption_happens_once_per_day(id: &str, day: u32) {
    let fixture = find_fixture(id, day, &[0], |failure, half| {
        failure.is_none_or(|failure| failure.elapsed > half)
    });
    let (mut runtime, class) = day_runtime(id, &fixture, day);
    let duration = class.duration();
    let first = runtime.active(&class, duration).unwrap();
    assert!(matches!(
        first.cause,
        Some(Cause::Graphics | Cause::Power)
    ));
    let remaining = duration - first.elapsed;
    let scenario = runtime.scenario().clone();
    let stock = Profile::uniform(scenario.topology(), Depth::default());
    let history = TruthState::cold(scenario.topology()).unwrap();
    let expected = scenario
        .draw(&class, &stock, &history, remaining, day, fixture.seed, 1)
        .unwrap();
    let second = runtime.active(&class, remaining).unwrap();
    // The suppressed day's remaining run reports only what truth itself draws.
    assert_eq!(second.cause, expected.map(|f| Cause::Truth(f.cause)));
    assert_eq!(second.elapsed, expected.map_or(remaining, |f| f.elapsed));
    assert_eq!(second.answer, expected.map(|_| EndAnswer::Cpu));
    // A new calendar day registers its own interruption again.
    runtime.set_day(day);
    assert!(!runtime.0.borrow().non_cpu_today);
}

#[test_case("successor-non-cpu-correct", 31; "graphics answered not CPU")]
#[test_case("successor-non-cpu-wrong", 121; "power answered CPU")]
#[test_case("successor-non-cpu-missing", 31; "graphics unanswered")]
fn an_earlier_truth_failure_beats_the_non_cpu_interruption(id: &str, day: u32) {
    let fixture = find_fixture(id, day, &DEEP, |failure, half| {
        failure.is_some_and(|failure| failure.elapsed < half)
    });
    let (mut runtime, class) = day_runtime(id, &fixture, day);
    let scenario = runtime.scenario().clone();
    let profile = Profile::uniform(
        scenario.topology(),
        Depth::new(fixture.depth).unwrap(),
    );
    let history = TruthState::cold(scenario.topology()).unwrap();
    let expected = scenario
        .draw(&class, &profile, &history, class.duration(), day, fixture.seed, 0)
        .unwrap()
        .unwrap();
    let event = runtime.active(&class, class.duration()).unwrap();
    assert_eq!(event.elapsed, expected.elapsed);
    assert_eq!(event.cause, Some(Cause::Truth(expected.cause)));
    // The policy-visible answer is a CPU failure whatever the scenario's non-CPU answer is.
    assert_eq!(event.answer, Some(EndAnswer::Cpu));
    // The day's non-CPU interruption was not consumed by the losing branch.
    assert!(!runtime.0.borrow().non_cpu_today);
}

#[test]
fn expired_deadline_refuses_active_time_without_fabricating_exposure() {
    let scenario = scenario("successor-edge-deep");
    let class = full_class(&scenario);
    let mut runtime = Runtime::new(scenario, SEED).unwrap();
    runtime.set_deadline(Some(Instant::now()));
    assert!(matches!(
        runtime.active(&class, class.duration()),
        Err(RunError::BudgetExpired)
    ));
    assert_eq!(runtime.now(), Duration::ZERO);
    assert_eq!(runtime.drain_observations().len(), 0);
}
