use super::*;

const SEED: u64 = 17;

fn scenario(id: &str) -> Scenario {
    Scenario::builtin(id, SEED).unwrap()
}

fn full_class(scenario: &Scenario) -> TrialClass {
    scenario.daily_mix().iter()
        .max_by_key(|block| block.class.loaded().len())
        .unwrap().class.clone()
}

fn profile(scenario: &Scenario, depths: PerCore<u8>) -> Profile {
    Profile::new(
        depths.iter().map(|(core, value)| (core, Depth::new(value).unwrap())).collect(),
        scenario.topology(),
    ).unwrap()
}

#[expect(clippy::disallowed_methods, reason = "tests write only simulator registers")]
fn set_core(machine: &mut Machine<'_>, core: CoreId, depth: u8) {
    machine.smu.set_offset(core, Depth::new(depth).unwrap().offset().unwrap()).unwrap();
}

#[expect(clippy::disallowed_methods, reason = "tests write only simulator registers")]
fn set_all(machine: &mut Machine<'_>, depth: u8) {
    machine.smu.set_all_offsets(Depth::new(depth).unwrap().offset().unwrap()).unwrap();
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
        assert_eq!(observation.night, false);
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
        set_core(&mut machine, info.core, if info.core.get() % 2 == 0 { 50 } else { 0 });
    }
    let depths = runtime.depths().unwrap();
    let profile = profile(&scenario, depths);
    let history = TruthState::cold(scenario.topology()).unwrap();
    let intended = class.duration();
    let expected = scenario.draw(&class, &profile, &history, intended, 1, SEED, 0)
        .unwrap().unwrap();
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
    let ObservationKind::Active { seconds, exposure, cause, launch, intended: requested, .. } = observation.kind else {
        panic!("expected active truth observation");
    };
    assert_eq!(requested, intended);
    assert_eq!(seconds, scenario.slice_seconds(
        &class, &profile, &history, expected.elapsed,
        Duration::from_secs(u64::from(class.regime().warm_up_s())),
    ).unwrap());
    assert_eq!(exposure, scenario.evaluate(&class, &profile, &history, expected.elapsed, 1).unwrap());
    assert_eq!(cause, Some(Cause::Truth(expected.cause)));
    assert_eq!(launch, true);
    for (core, depth) in depths.iter() {
        if depth == 0 {
            assert_eq!(exposure.co_per_core.get(core), Some(0.0));
        }
    }
}

#[test]
fn long_daily_runs_checkpoint_without_restarting_onset_or_recounting_launches() {
    let scenario = scenario("successor-weak-core-delayed");
    let class = full_class(&scenario).with_duration(Duration::from_mins(10)).unwrap();
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
        let ObservationKind::Active { seconds, exposure, launch, intended: requested, .. } = observation.kind else {
            panic!("expected active truth observation");
        };
        assert_eq!(launch, index == 0);
        assert_eq!(observation.before, observed);
        assert_eq!(seconds.intended, class.duration());
        assert_eq!(seconds, scenario.slice_seconds(
            &class, &profile, &history, seconds.observed,
            Duration::from_secs(u64::from(class.regime().warm_up_s())),
        ).unwrap());
        assert_eq!(exposure, scenario.evaluate(&class, &profile, &history, seconds.observed, 1).unwrap());
        history.advance_profile(Some(&class), &profile, seconds.observed).unwrap();
        observed += seconds.observed;
        if launch { intended += requested; }
        assert_eq!(observation.after, observed);
    }
    assert_eq!(observed, Duration::from_mins(25));
    assert_eq!(intended, Duration::from_mins(25));
}

#[test]
fn transient_profile_changes_reset_onset_even_when_the_original_profile_is_restored() {
    let scenario = scenario("successor-weak-core-delayed");
    let class = full_class(&scenario).with_duration(Duration::from_mins(10)).unwrap();
    let mut runtime = Runtime::new(scenario.clone(), SEED).unwrap();
    let mut machine = runtime.seams();
    runtime.active(&class, Duration::from_mins(10)).unwrap();
    runtime.drain_observations();
    set_core(&mut machine, scenario.topology()[0].core, 1);
    set_core(&mut machine, scenario.topology()[0].core, 0);
    runtime.active(&class, Duration::from_mins(10)).unwrap();
    let observations = runtime.drain_observations();
    assert_eq!(observations.len(), 3);
    assert!(matches!(observations[0].kind, ObservationKind::ProfileChange));
    assert!(matches!(observations[1].kind, ObservationKind::ProfileChange));
    let ObservationKind::Active { seconds, .. } = &observations[2].kind else {
        panic!("expected active truth observation");
    };
    let stock = Profile::uniform(scenario.topology(), Depth::default());
    let history = TruthState::cold(scenario.topology()).unwrap();
    assert_eq!(*seconds, scenario.slice_seconds(
        &class, &stock, &history, Duration::from_mins(10),
        Duration::from_secs(u64::from(class.regime().warm_up_s())),
    ).unwrap());
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
    let class = full_class(&scenario).with_duration(Duration::from_mins(20)).unwrap();
    let mut runtime = Runtime::new(scenario.clone(), SEED).unwrap();
    let mut machine = runtime.seams();
    set_all(&mut machine, 25);
    let depths = runtime.depths().unwrap();
    let profile = profile(&scenario, depths);
    runtime.drain_observations();
    runtime.begin_night(Duration::from_mins(5));
    let cancel = CancellationToken::new();
    let spec = class.to_spec("one-night-trial".into(), PerCore::default(), 0, SEED).unwrap();
    let ready = machine.trials.sweep(&cancel).unwrap().stopped;
    let mut running = machine.trials.start(&cancel, spec, ready)
        .map_err(|partial| partial.error).unwrap();
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
    assert_eq!(observation.night, true);
    assert_eq!(observation.after - observation.before, ran);
    let ObservationKind::Active { exposure, trial_id, cause, intended, .. } = &observation.kind else {
        panic!("expected active truth observation");
    };
    assert_eq!(trial_id.as_deref(), Some("one-night-trial"));
    assert_eq!(*intended, class.duration());
    let mut history = TruthState::cold(scenario.topology()).unwrap();
    assert_eq!(*exposure, scenario.evaluate(&class, &profile, &history, ran, 1).unwrap());
    history.advance_profile(Some(&class), &profile, ran).unwrap();
    // Either the truth failure or the operator-ended allocation interrupts the run.
    history.interrupt();
    assert_eq!(cause.is_some(), ran < Duration::from_mins(5));
    runtime.sleep(Duration::from_mins(30)).unwrap();
    history.advance_profile(None, &profile, Duration::from_mins(30)).unwrap();
    let sleep = runtime.drain_observations().pop().unwrap();
    assert!(matches!(sleep.kind, ObservationKind::Sleep));
    assert_eq!(sleep.night, false);
    assert_eq!(sleep.before, ran);
    assert_eq!(sleep.after, ran + Duration::from_mins(30));
    assert_eq!(sleep.profile, depths);
    let event = runtime.active(&class, Duration::from_mins(1)).unwrap();
    let resumed = runtime.drain_observations().pop().unwrap();
    let ObservationKind::Active { exposure, .. } = resumed.kind else {
        panic!("expected resumed active observation");
    };
    assert_eq!(resumed.before, sleep.after);
    assert_eq!(resumed.after - resumed.before, event.elapsed);
    assert_eq!(
        exposure,
        scenario.evaluate(&class, &profile, &history, event.elapsed, 1).unwrap(),
    );
    runtime.firmware_changed().unwrap();
    let stock: PerCore<u8> = scenario.topology().iter().map(|info| (info.core, 0)).collect();
    assert_eq!(runtime.depths().unwrap(), stock);
    let reset = runtime.drain_observations().pop().unwrap();
    assert_eq!(reset.profile, stock);
    assert_eq!(reset.before, reset.after);
    assert!(matches!(reset.kind, ObservationKind::ProfileChange));
}

#[test]
fn expired_deadline_refuses_active_time_without_fabricating_exposure() {
    let scenario = scenario("successor-edge-deep");
    let class = full_class(&scenario);
    let mut runtime = Runtime::new(scenario, SEED).unwrap();
    runtime.set_deadline(Some(Instant::now()));
    assert!(matches!(runtime.active(&class, class.duration()), Err(RunError::BudgetExpired)));
    assert_eq!(runtime.now(), Duration::ZERO);
    assert_eq!(runtime.drain_observations().len(), 0);
}
