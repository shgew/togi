//! The Ruleset 13 narration on hand-built states: what a night trial is for, between trials, the day state, the
//! stock failure stop and recovery.

use super::*;

fn running(fx: &mut Fx, trial: TrialView) -> super::super::Story {
    fx.trial = Some(trial);
    story(&fx.scene(ts(1001)))
}

#[test]
fn a_mapping_trial_names_its_ccd_offset_and_task() {
    let mut fx = Fx::new();
    fx.policy.night = Some(night(vec![
        planned(Queue::Mapping, 1, TaskStatus::Running),
        planned(Queue::Mapping, 2, TaskStatus::Pending),
        planned(Queue::Control, 3, TaskStatus::Pending),
    ]));
    let said = running(&mut fx, mapping(1, -15));
    assert_eq!(said.label, "MAPPING");
    assert_eq!(said.paint, Paint::Running);
    assert!(said.lines[0].starts_with("Mapping CCD 1 at -15: only that CCD runs at -15"));
    assert!(said.lines[0].contains("every other CCD stays at CO 0"));
    assert!(
        said.lines[1].contains("2 tasks wait behind it."),
        "{:?}",
        said.lines
    );
    assert_eq!(said.brief, "Mapping CCD 1 at -15 (mapping task 1).");
}

#[test]
fn a_retried_trial_says_it_could_not_run_before() {
    let mut fx = Fx::new();
    let mut retried = mapping(0, -10);
    retried.retry = true;
    let said = running(&mut fx, retried);
    assert!(said.lines[1].ends_with("This is a retry of a trial that could not run."));
    let said = running(&mut fx, mapping(0, -10));
    assert!(!said.lines[1].contains("retry"));
}

#[test]
fn a_qualification_trial_names_the_profile_and_its_progress() {
    let mut fx = Fx::new();
    let mut plan = night(vec![planned(Queue::Qualification, 1, TaskStatus::Running)]);
    plan.qualification = Some(QualificationProgress {
        profile: profile(&[(0, -10), (1, -5)]),
        outstanding: Vec::new(),
        witnessed: 3,
    });
    fx.policy.night = Some(plan);
    let mut trial = trial(Regime::R1, &[0, 1]);
    trial.activity = Some(Activity::Qualification);
    trial.tested = Some(Tested::Exact {});
    trial.task = Some(task(Queue::Qualification, 1));
    let said = running(&mut fx, trial.clone());
    assert_eq!(said.label, "QUALIFYING");
    assert!(
        said.lines[0].contains("the exact profile, CCD 0 at -10, CCD 1 at -5"),
        "{:?}",
        said.lines
    );
    assert!(said.lines[1].starts_with("3 cells witnessed, 0 outstanding;"));
    trial.task = Some(task(Queue::Requalification, 2));
    assert_eq!(running(&mut fx, trial).label, "REQUALIFYING");
}

#[test]
fn a_reproduction_and_a_control_say_what_they_do_not_do() {
    let mut fx = Fx::new();
    let mut repro = trial(Regime::R1, &[0, 1]);
    repro.activity = Some(Activity::Reproduction);
    repro.tested = Some(Tested::Exact {});
    let said = running(&mut fx, repro);
    assert_eq!(said.label, "REPRODUCING");
    assert!(said.lines[0].contains("clears nothing and earns no qualification credit"));
    let mut control = trial(Regime::R3, &[0, 1]);
    control.activity = Some(Activity::Control);
    control.tested = Some(Tested::Exact {});
    let said = running(&mut fx, control);
    assert_eq!(said.label, "CONTROLS");
    assert!(said.lines[0].contains("every CCD at CO 0"));
}

#[test]
fn a_trial_without_a_night_activity_claims_no_purpose() {
    let mut fx = Fx::new();
    let mut old = trial(Regime::R1, &[0]);
    old.activity = Some(Activity::Search);
    let said = running(&mut fx, old);
    assert_eq!(said.label, "TOGI");
    assert!(said.lines[0].contains("names no night task"));
}

#[test]
fn between_trials_the_night_names_its_next_task_and_never_a_forecast() {
    let mut fx = Fx::new();
    fx.last = Some(end_view(Outcome::Failure, None));
    fx.policy.night = Some(night(vec![
        planned(Queue::Mapping, 1, TaskStatus::Complete),
        planned(Queue::Mapping, 2, TaskStatus::Pending),
        planned(Queue::Control, 3, TaskStatus::Pending),
    ]));
    let said = story(&fx.scene(ts(2000)));
    assert_eq!(said.label, "NIGHT");
    assert_eq!(said.lines[0], "Trial t0 ended: failure.");
    assert_eq!(
        said.lines[1],
        "2 tasks left in the night's plan; the next listed is mapping task 2."
    );
    for forecast in ["will", "expect", "estimate", "finish", "hours left"] {
        assert!(!super::said(&said).contains(forecast), "{forecast}");
    }
}

#[test]
fn an_open_night_names_the_backends_it_cannot_use() {
    let mut fx = Fx::new();
    fx.last = Some(end_view(Outcome::Pass, None));
    let mut plan = night(vec![planned(Queue::Mapping, 2, TaskStatus::Pending)]);
    plan.unavailable = vec![togi_journal::UnavailableBackend {
        backend: togi_machine::Backend::Ycruncher,
        causes: vec![togi_journal::Seq::new(3).expect("a seq")],
    }];
    fx.policy.night = Some(plan);
    let said = story(&fx.scene(ts(2000)));
    assert_eq!(
        said.lines[1],
        "1 task left in the night's plan; the next listed is mapping task 2. Unavailable this night: ycruncher."
    );
}

#[test]
fn an_open_night_with_no_task_left_runs_filler_and_a_closed_one_gives_way_to_the_day() {
    let mut fx = Fx::new();
    fx.last = Some(end_view(Outcome::Pass, None));
    fx.policy.night = Some(night(vec![
        planned(Queue::Mapping, 1, TaskStatus::Complete),
        planned(Queue::Control, 2, TaskStatus::Cancelled),
    ]));
    let said = story(&fx.scene(ts(2000)));
    assert_eq!(said.label, "NIGHT");
    assert_eq!(
        said.lines[1],
        "Every planned task is done; filler work runs until you end the night."
    );
    if let Some(night) = fx.policy.night.as_mut() {
        night.closed_by = Some(togi_journal::Seq::new(40).expect("a seq"));
    }
    let said = story(&fx.scene(ts(2000)));
    assert_eq!(said.label, "NIGHT OVER");
    assert_eq!(
        said.lines[1],
        "No day profile has been applied; every CCD stays at CO 0."
    );
}

#[test]
fn the_day_state_names_the_applied_profile_and_what_the_latest_proposal_says() {
    let mut fx = Fx::new();
    fx.last = Some(end_view(Outcome::Pass, None));
    let said = story(&fx.scene(ts(2000)));
    assert_eq!(said.label, "TOGI");
    assert_eq!(
        said.lines[1],
        "No day profile has been applied; every CCD stays at CO 0."
    );
    fx.policy.applied = Some(profile(&[(0, -10), (1, -5)]));
    fx.policy.day = DayProjection {
        hold: Some(togi_tuner::policy::HoldProjection {
            profile: profile(&[(0, -10), (1, -5)]),
            eligible_s: 5400,
            required_s: 180_000,
            witness: togi_journal::Seq::new(9).expect("a seq"),
        }),
        freeze: true,
        firmware: FirmwareValidity::Valid,
        ..fx.policy.day.clone()
    };
    let said = story(&fx.scene(ts(2000)));
    assert_eq!(said.label, "DAY");
    assert_eq!(
        said.lines[1],
        "The day profile is CCD 0 at -10, CCD 1 at -5. 1.500 h credited at it."
    );
    assert_eq!(said.lines[2], "Promotion is frozen.");
    assert!(
        !super::said(&said).contains("required"),
        "no threshold is narrated: {:?}",
        said.lines
    );
}

#[test]
fn a_firmware_change_says_the_day_profile_goes_back_to_zero() {
    let mut fx = Fx::new();
    fx.last = Some(end_view(Outcome::Pass, None));
    fx.policy.applied = Some(profile(&[(0, -10)]));
    fx.policy.day.firmware = FirmwareValidity::Invalidated {
        since: togi_journal::Seq::new(4).expect("a seq"),
    };
    let said = story(&fx.scene(ts(2000)));
    assert!(said.lines[2].contains("A firmware change invalidated the qualifications"));
}

#[test]
fn a_stock_failure_is_not_blamed_on_curve_optimizer() {
    let mut fx = Fx::new();
    fx.policy.stopped = true;
    let scene = fx.scene(ts(10));
    let said = story(&scene);
    assert_eq!(said.label, "STOCK FAILURE");
    assert_eq!(said.paint, Paint::Bad);
    assert!(said.lines[0].contains("every CCD at CO 0"));
    assert!(said.lines[0].contains("Curve Optimizer did not cause it"));
    assert!(said.lines[1].contains("explicit reset and fresh controls"));
    let band = resting_band(&scene);
    assert_eq!(band.title, "STOCK FAILURE");
    assert_eq!(band.paint, Paint::Bad);
    fx.dead_end = Some(DeadEndView {
        at: ts(1),
        condition: DeadEndCondition::Smu,
        detail: String::new(),
    });
    assert_eq!(
        story(&fx.scene(ts(10))).label,
        "DEAD END",
        "a dead end speaks first"
    );
}

#[test]
fn a_recovery_says_the_journal_is_intact_and_what_the_night_does_next() {
    let mut fx = Fx::new();
    fx.recover = Some(RecoveryView {
        crash_at: ts(50),
        reset: None,
        trial: Some(trial(Regime::R1, &[0])),
        end: None,
    });
    fx.policy.night = Some(night(vec![planned(Queue::Mapping, 5, TaskStatus::Pending)]));
    let said = story(&fx.scene(ts(100)));
    assert_eq!(said.label, "RECOVERED");
    assert_eq!(said.paint, Paint::Warn);
    assert_eq!(
        said.lines[0],
        "I picked up from the journal after the reboot; nothing it recorded is lost."
    );
    assert_eq!(
        said.lines[1],
        "1 task left in the night's plan; the next listed is mapping task 5."
    );
}
