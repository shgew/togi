use test_case::test_case;

use jiff::SignedDuration;
use togi_journal::{
    Activity, CycleEvent, Decision, Event, FactSource, FailureCarried, HuntStage, Inference,
    Outcome, Payload, Stamp, TrialCarried, TrialClass, TrialIntent, TunerWarning, WarningKind,
};
use togi_machine::{BootId, Condition, PerCore, Regime, Signal};

use super::support::{
    EPOCH, Log, build, cycle, decision, failure, hunt_group, hunt_start, intent, nz, profile, seq,
    set, trial, workload,
};
use crate::evidence::{pass_counts, single_carried_failure_decisions};
use crate::project::Projection;
use crate::report::compute;

fn target_intent() -> TrialIntent {
    let mut target = intent("target", Regime::R7, &[1, 0], &[-10, -10]);
    target.condition = Condition::Parked;
    target
}

type Past = (Vec<i64>, Outcome, fn(&mut TrialIntent));

fn nothing(_: &mut TrialIntent) {}

fn carry_trial(log: &mut Log, planned: &TrialIntent, outcome: Outcome) -> togi_journal::Seq {
    let mut fact = carried(outcome);
    fact.class = TrialClass {
        regime: planned.regime,
        workload: Some(planned.workload),
        cores: crate::project::loaded(planned),
        duration_s: planned.duration_s,
    };
    fact.profile = planned.profile;
    fact.condition = planned.condition;
    log.add(fact)
}

fn scheduled_trial(
    log: &mut Log,
    mut planned: TrialIntent,
    outcome: Outcome,
    blamed: Option<togi_machine::CoreId>,
) -> togi_journal::Seq {
    let hunt = u32::try_from(log.events.len() + 1).unwrap();
    let loaded = crate::project::loaded(&planned);
    let mut start = hunt_start(hunt, &planned.trial.to_string(), 1);
    start.regime = planned.regime;
    start.workload = planned.workload;
    start.cores = loaded;
    start.candidates = loaded;
    start.failing = planned.profile;
    start.parked = planned.profile;
    start.duration_s = planned.duration_s;
    start.trial_s = planned.duration_s;
    start.ranking = planned.profile.iter().map(|(core, _)| core).collect();
    log.add(start);
    let mut group = hunt_group(hunt, 1, HuntStage::Part);
    group.cores = loaded;
    group.set = loaded;
    group.profile = planned.profile;
    group.duration_s = planned.duration_s;
    log.add(group);
    planned.hunt = Some(nz(hunt));
    planned.group = Some(nz(1));
    planned.condition = Condition::Parked;
    planned.activity = Some(Activity::Hunt);
    let mut attribution = failure(Some(&planned.trial.to_string()));
    attribution.condition = Some(planned.condition);
    attribution.regime = Some(planned.regime);
    attribution.profile = Some(planned.profile);
    attribution.core = blamed;
    if let Some(core) = blamed {
        attribution.attribution = togi_journal::Attribution::Attributed;
        attribution.offset = planned.profile.get(core);
    }
    let ended = trial(log, planned, outcome);
    if outcome == Outcome::Failure {
        log.add(attribution);
    }
    togi_tuner::replay(&log.events).unwrap();
    ended
}

fn prior(history: &[Past], before: Option<usize>) -> usize {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    let mut ends = Vec::new();
    for (i, (offsets, outcome, modify)) in history.iter().enumerate() {
        let mut past = target_intent();
        past.trial = format!("h{i}").into();
        past.profile = profile(offsets);
        modify(&mut past);
        ends.push(carry_trial(&mut log, &past, *outcome));
    }
    let target = log.add(target_intent());
    let before = before.map_or(target, |index| ends[index]);
    let journal = log.journal();
    let p = Projection::new(&journal);
    let at = p.trials.len() - 1;
    pass_counts(&p, vec![(before, at)])[&(before, at)].unwrap()
}

#[test_case(&[(vec![-10, -10], Outcome::Pass, nothing), (vec![-11, -10], Outcome::Pass, nothing)], None, 2; "equal and deeper count")]
#[test_case(&[(vec![-9, -10], Outcome::Pass, nothing), (vec![-11, -9], Outcome::Pass, nothing)], None, 0; "shallow and incomparable do not")]
#[test_case(&[(vec![-11, -11], Outcome::Pass, nothing), (vec![-9, -9], Outcome::Failure, nothing), (vec![-10, -10], Outcome::Pass, nothing)], None, 1; "later shallow failure invalidates")]
#[test_case(&[(vec![-10, -10], Outcome::Pass, nothing), (vec![-11, -11], Outcome::Failure, nothing)], None, 1; "deeper failure does not invalidate")]
#[test_case(&[(vec![-10, -10], Outcome::Pass, |t| t.condition = Condition::Together)], None, 1; "same cores and another condition")]
#[test_case(&[
    (vec![-10, -10], Outcome::Pass, |t| t.duration_s = 300),
    (vec![-10, -10], Outcome::Pass, |t| t.workload = workload(Regime::R7, 1)),
    (vec![-10, -10], Outcome::Pass, |t| { t.regime = Regime::R6; t.workload = workload(Regime::R6, 0); }),
    (vec![-10, -10], Outcome::Pass, |t| t.cores = set(&[0])),
], None, 0; "class fields matter")]
#[test_case(&[(vec![-10, -10], Outcome::Pass, nothing), (vec![-10, -10], Outcome::Pass, nothing)], Some(0), 0; "hunt boundary is excluded")]
#[test_case(&[(vec![-10, -10], Outcome::Pass, nothing), (vec![-9, -9], Outcome::Inconclusive, nothing)], None, 1; "inconclusive does not invalidate")]
fn passes_count_what_is_as_deep_and_not_ruled_out_by_a_failure(
    history: &[Past],
    before: Option<usize>,
    want: usize,
) {
    assert_eq!(prior(history, before), want);
}

fn idle_crash(regime: Regime, loaded: &[i64], idle_profile: Option<&[i64]>) -> usize {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    let planned = |id: &str, hunt: Option<u32>| {
        let mut planned = intent(id, regime, loaded, &[-10, -10]);
        planned.hunt = hunt.map(nz);
        planned.group = hunt.map(nz);
        planned.duration_s = 120;
        planned
    };
    carry_trial(&mut log, &planned("pass", None), Outcome::Pass);
    let mut crash = failure(None);
    crash.signal = Signal::Crash;
    crash.condition = Some(Condition::Together);
    crash.profile = idle_profile.map(profile);
    log.add(crash);
    log.add(hunt_start(1, "t", 1));
    log.add(hunt_group(1, 1, HuntStage::Part));
    trial(&mut log, planned("group", Some(1)), Outcome::Pass);
    let journal = log.journal();
    compute(&Projection::new(&journal), None).evidence.groups[0]
        .prior
        .unwrap()
}

#[test]
fn an_idle_crash_invalidates_the_passes_of_an_all_core_r6_target_it_is_no_deeper_than() {
    let r6 = Regime::R6;
    assert_eq!(
        idle_crash(r6, &[0, 1], None),
        0,
        "tuner infers the applied profile"
    );
    assert_eq!(idle_crash(r6, &[0, 1], Some(&[-10, -10])), 0, "equal");
    assert_eq!(idle_crash(r6, &[0, 1], Some(&[-9, -10])), 0, "shallower");
    assert_eq!(idle_crash(r6, &[0, 1], Some(&[-11, -10])), 1, "deeper");
    assert_eq!(idle_crash(r6, &[0], Some(&[-10, -10])), 1, "partial R6");
    assert_eq!(idle_crash(Regime::R7, &[0, 1], Some(&[-10, -10])), 1, "R7");
}

#[test]
fn a_known_failure_skip_does_not_recount_an_idle_failure_or_invalidate_a_later_pass() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    let planned = |id: &str, hunt: Option<u32>| {
        let mut planned = intent(id, Regime::R6, &[0, 1], &[-10, -10]);
        planned.hunt = hunt.map(nz);
        planned.group = hunt.map(nz);
        planned
    };
    let mut original = failure(None);
    original.signal = Signal::Crash;
    original.regime = Some(Regime::R6);
    original.condition = Some(Condition::Together);
    original.profile = Some(profile(&[-9, -10]));
    let original_seq = log.add(original.clone());
    carry_trial(&mut log, &planned("pass", None), Outcome::Pass);
    original.known_failure = Some(original_seq);
    log.add(original);
    log.add(hunt_start(1, "t", 1));
    log.add(hunt_group(1, 1, HuntStage::Part));
    trial(&mut log, planned("group", Some(1)), Outcome::Pass);
    let journal = log.journal();
    let p = Projection::new(&journal);
    let m = compute(&p, None);
    assert_eq!(
        m.failures.counts.into_iter().collect::<Vec<_>>(),
        [(
            (
                Some(Regime::R6),
                None,
                Signal::Crash,
                togi_journal::Attribution::Unattributed
            ),
            1
        )]
    );
    assert_eq!(m.evidence.groups[0].prior, Some(1));
}

#[test]
fn a_failure_after_a_prior_pass_is_listed_by_class() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    scheduled_trial(
        &mut log,
        intent("a", Regime::R6, &[0, 1], &[-10, -10]),
        Outcome::Pass,
        None,
    );
    scheduled_trial(
        &mut log,
        intent("b", Regime::R6, &[0, 1], &[-10, -10]),
        Outcome::Failure,
        None,
    );
    scheduled_trial(
        &mut log,
        intent("c", Regime::R6, &[0, 1], &[-11, -11]),
        Outcome::Failure,
        None,
    );
    let journal = log.journal();
    let p = Projection::new(&journal);
    let m = compute(&p, None);
    assert_eq!(
        m.evidence
            .contradictions
            .into_iter()
            .map(|(class, count)| (class.label(), count))
            .collect::<Vec<_>>(),
        [("R6/mprime-sse-4k-21k-idle/00,01/120s".to_owned(), 1)]
    );
}

#[test]
fn a_backend_change_excludes_old_passes_from_contradictions() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    scheduled_trial(
        &mut log,
        intent("old", Regime::R6, &[0, 1], &[-10, -10]),
        Outcome::Pass,
        None,
    );
    let mut loaded = super::support::config_loaded(build("test", None));
    loaded.config.backends.mprime = Some("/nix/store/new-mprime".into());
    log.add(loaded);
    scheduled_trial(
        &mut log,
        intent("new", Regime::R6, &[0, 1], &[-10, -10]),
        Outcome::Failure,
        None,
    );
    let journal = log.journal();
    assert!(
        compute(&Projection::new(&journal), None)
            .evidence
            .contradictions
            .is_empty()
    );
}

#[test_case(None, 1; "carried pass is admitted")]
#[test_case(Some(Outcome::Failure), 0; "carried failure invalidates older pass")]
#[test_case(Some(Outcome::Inconclusive), 1; "inconclusive carry does not invalidate")]
fn carried_observations_use_the_tuners_admission(outcome: Option<Outcome>, expected: usize) {
    let mut log = Log::new(&[(0, 0)]);
    log.add(carried(Outcome::Pass));
    if let Some(outcome) = outcome {
        log.add(carried(outcome));
    }
    let mut planned = intent("target", Regime::R6, &[0], &[-10]);
    planned.duration_s = 60;
    let before = log.add(planned);
    let journal = log.journal();
    let p = Projection::new(&journal);
    assert_eq!(
        pass_counts(&p, vec![(before, 0)])[&(before, 0)],
        Some(expected)
    );
}

#[test]
fn a_later_backend_change_does_not_rewrite_historical_counts() {
    let mut log = Log::new(&[(0, 0)]);
    let planned = |name| {
        let mut planned = intent(name, Regime::R6, &[0], &[-10]);
        planned.duration_s = 60;
        planned
    };
    scheduled_trial(&mut log, planned("old"), Outcome::Pass, None);
    let before = log.add(planned("target"));
    let mut loaded = super::support::config_loaded(build("test", None));
    loaded.config.backends.mprime = Some("/nix/store/new-mprime".into());
    log.add(loaded);
    let journal = log.journal();
    let p = Projection::new(&journal);
    assert_eq!(pass_counts(&p, vec![(before, 1)])[&(before, 1)], Some(1));
}

#[test_case(false, 0; "different carried backend is excluded")]
#[test_case(true, 1; "matching carried backend is admitted")]
fn carried_passes_keep_their_recorded_backend(matching: bool, expected: usize) {
    let mut log = Log::new(&[(0, 0)]);
    let mut pass = carried(Outcome::Pass);
    pass.source.backend = Some(togi_journal::BackendIdentity {
        backend: togi_machine::Backend::Mprime,
        path: "/nix/store/carried-mprime".into(),
    });
    log.add(pass);
    if matching {
        let mut loaded = super::support::config_loaded(build("test", None));
        loaded.config.backends.mprime = Some("/nix/store/carried-mprime".into());
        log.add(loaded);
    }
    let mut planned = intent("target", Regime::R6, &[0], &[-10]);
    planned.duration_s = 60;
    let before = log.add(planned);
    let journal = log.journal();
    let p = Projection::new(&journal);
    assert_eq!(
        pass_counts(&p, vec![(before, 0)])[&(before, 0)],
        Some(expected)
    );
}

#[test]
fn hunt_reuse_reports_admitted_carried_passes() {
    let mut log = Log::new(&[(0, 0)]);
    log.add(carried(Outcome::Pass));
    let mut start = hunt_start(1, "source", 1);
    start.regime = Regime::R6;
    start.workload = workload(Regime::R6, 0);
    start.duration_s = 60;
    start.trial_s = 60;
    log.add(start);
    log.add(hunt_group(1, 1, HuntStage::Part));
    let mut planned = intent("group", Regime::R6, &[0], &[-10]);
    planned.duration_s = 60;
    planned.hunt = Some(nz(1));
    planned.group = Some(nz(1));
    trial(&mut log, planned, Outcome::Pass);
    let journal = log.journal();
    let metrics = compute(&Projection::new(&journal), None).evidence;
    assert_eq!(metrics.groups[0].prior, Some(1));
    assert_eq!(metrics.groups[0].established(), Some(true));
}

#[test]
fn a_since_cutoff_keeps_earlier_admitted_passes_for_later_hunts_and_failures() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    carry_trial(&mut log, &target_intent(), Outcome::Pass);
    scheduled_trial(log.at(100), target_intent(), Outcome::Failure, None);
    let journal = log.journal();
    let p = Projection::new(&journal);
    let all = compute(&p, None).evidence;
    let recent = compute(&p, Some(EPOCH + SignedDuration::from_secs(50))).evidence;
    for metrics in [&all, &recent] {
        assert_eq!(metrics.unavailable, 0);
        assert_eq!(metrics.groups.len(), 1);
        assert_eq!(metrics.groups[0].prior, Some(1));
        assert_eq!(metrics.groups[0].established(), Some(true));
        assert_eq!(metrics.contradictions.len(), 1);
        assert_eq!(metrics.contradictions.values().next(), Some(&1));
    }
    assert_eq!(all.groups, recent.groups);
    assert_eq!(all.stages, recent.stages);
    assert_eq!(all.contradictions, recent.contradictions);
}

#[test]
fn resetting_a_blamed_unloaded_core_revives_the_passes_the_tuner_keeps() {
    let mut log = Log::new(&[(0, 0), (1, 0), (2, 0)]);
    scheduled_trial(
        &mut log,
        intent("pass", Regime::R7, &[0, 1], &[-10, -10, 0]),
        Outcome::Pass,
        None,
    );
    scheduled_trial(
        &mut log,
        intent("failure", Regime::R7, &[0, 1], &[-9, -9, 0]),
        Outcome::Failure,
        Some(super::support::core(2)),
    );
    let before_reset = log.add(super::support::trial_start("boundary"));
    log.add(togi_journal::CommandReset {
        target: togi_journal::ResetTarget::Core(super::support::core(2)),
    });
    let after_reset = log.add(intent("target", Regime::R7, &[0, 1], &[-10, -10, 0]));
    let journal = log.journal();
    let p = Projection::new(&journal);
    let at = p.trials.len() - 1;
    let counts = pass_counts(&p, vec![(before_reset, at), (after_reset, at)]);
    assert_eq!(counts[&(before_reset, at)], Some(0));
    assert_eq!(counts[&(after_reset, at)], Some(1));
}

#[test]
fn a_journal_the_tuner_refuses_reports_unavailable_evidence_not_zero() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    trial(&mut log, target_intent(), Outcome::Pass);
    let mut target = target_intent();
    target.trial = "failed".to_owned().into();
    trial(&mut log, target, Outcome::Failure);
    let journal = log.journal();
    let metrics = compute(&Projection::new(&journal), None).evidence;
    assert_eq!(metrics.unavailable, 1);
    assert!(metrics.contradictions.is_empty());
    assert!(log.report().contains("prior evidence queries unavailable"));
}

#[test]
fn warnings_are_counted_by_kind_and_a_cutoff_drops_the_earlier_ones() {
    let mut log = Log::new(&[(0, 0)]);
    let warning = || TunerWarning {
        warning: WarningKind::Monotonicity,
        trial: None,
        passes: Vec::new(),
        detail: String::new(),
    };
    log.at(0).add(warning());
    log.at(100).add(warning());
    log.add(cycle(1, CycleEvent::Start, false));
    let journal = log.journal();
    let p = Projection::new(&journal);
    let count = |since| {
        compute(&p, since)
            .evidence
            .warnings
            .get(&WarningKind::Monotonicity)
            .copied()
    };
    assert_eq!(count(None), Some(2));
    assert_eq!(count(Some(EPOCH + SignedDuration::from_secs(50))), Some(1));
    assert_eq!(count(Some(EPOCH + SignedDuration::from_secs(500))), None);
}

#[test]
fn the_failure_rate_by_depth_lists_multi_core_classes_that_failed() {
    let mut log = Log::new(&[(0, 0), (1, 0), (2, 1)]);
    let planned = |id: &str, cores: &[i64], offsets: &[i64]| {
        let mut planned = intent(id, Regime::R7, cores, offsets);
        planned.activity = Some(Activity::Checking);
        planned
    };
    trial(
        &mut log,
        planned("a", &[0, 1], &[-10, -20, 0]),
        Outcome::Pass,
    );
    trial(
        &mut log,
        planned("b", &[0, 1], &[-10, -20, 0]),
        Outcome::Failure,
    );
    trial(
        &mut log,
        planned("c", &[0, 1], &[-5, -20, 0]),
        Outcome::Pass,
    );
    trial(&mut log, planned("d", &[0, 2], &[-5, 0, -7]), Outcome::Pass);
    trial(&mut log, planned("e", &[0], &[-5, 0, 0]), Outcome::Failure);
    let journal = log.journal();
    let rates = compute(&Projection::new(&journal), None).depth;
    let labels: Vec<_> = rates.iter().map(|r| r.class.label()).collect();
    let got: Vec<_> = rates
        .iter()
        .zip(&labels)
        .map(|(r, label)| (label.as_str(), r.depth, r.trials, r.failures))
        .collect();
    assert_eq!(
        got,
        [
            ("R7/mprime-avx2-36k-248k-allcore/00,01/120s", -10, 2, 1),
            ("R7/mprime-avx2-36k-248k-allcore/00,01/120s", -5, 1, 0),
        ]
    );
}

/// `n` is the event's seq; the event holds `payload` and cites `cause`.
fn event(n: u64, seconds: i64, cause: &[u64], payload: impl Into<Payload>) -> Event {
    Event::new(
        seq(n),
        Stamp {
            time: EPOCH + SignedDuration::from_secs(seconds),
            mono: None,
        },
        BootId::from("a".to_owned()),
        cause.iter().copied().map(seq).collect(),
        payload.into(),
    )
}

fn source(session: &str, number: u64) -> FactSource {
    FactSource {
        session: session.parse().unwrap(),
        seq: seq(number),
        build: build("test", None),
        trial: None,
        time: EPOCH,
        boot: BootId::from("a".to_owned()),
        backend: None,
    }
}

pub(super) fn carried(outcome: Outcome) -> TrialCarried {
    TrialCarried {
        source: source("20251231T000000Z", 1),
        class: TrialClass {
            regime: Regime::R6,
            workload: Some(workload(Regime::R6, 0)),
            cores: set(&[0]),
            duration_s: 60,
        },
        condition: Condition::Together,
        activity: None,
        rerun: false,
        profile: profile(&[-10]),
        outcome,
        signal: (outcome == Outcome::Failure).then_some(Signal::ComputationError),
        duration_s: 60,
        core: None,
        stalled_core: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: togi_machine::CoreSet::default(),
        ccd_mhz: std::collections::BTreeMap::new(),
    }
}

fn carried_idle_failure() -> FailureCarried {
    FailureCarried {
        source: source("20251231T000000Z", 2),
        class: TrialClass {
            regime: Regime::R6,
            workload: None,
            cores: set(&[0]),
            duration_s: 0,
        },
        failure: failure(None),
    }
}

fn inferred(inference: Inference) -> togi_journal::HuntGroup {
    let mut group = hunt_group(1, 1, HuntStage::Part);
    group.inferred = Some(inference);
    group
}

fn pool(decided_at: i64, payload: impl Into<Payload>, cause: &[u64]) -> Vec<Event> {
    vec![
        event(1, -3600, &[], carried(Outcome::Failure)),
        event(2, -3600, &[], carried_idle_failure()),
        event(3, -3600, &[], failure(None)),
        event(4, -3600, &[], carried(Outcome::Pass)),
        event(5, -3600, &[], hunt_start(1, "t", 1)),
        event(6, decided_at, cause, payload),
    ]
}

#[test_case(inferred(Inference::Failure).into(), &[5, 1], None, 1; "inferred failure with hunt context")]
#[test_case(decision(Decision::Backoff, 0, -10, -9).into(), &[2], None, 1; "carried idle failure backoff")]
#[test_case(inferred(Inference::Failure).into(), &[1, 1], None, 1; "repeated cause is one observation")]
#[test_case(inferred(Inference::Failure).into(), &[1, 2], None, 0; "two carried failures")]
#[test_case(decision(Decision::Backoff, 0, -10, -9).into(), &[1, 3], None, 0; "carried and live failures")]
#[test_case(decision(Decision::Backoff, 0, -10, -9).into(), &[3], None, 0; "live failure only")]
#[test_case(inferred(Inference::Pass).into(), &[1, 4, 5], None, 0; "passing inference with context failure")]
#[test_case(hunt_start(1, "t", 1).into(), &[1], None, 0; "hunt start context citation")]
#[test_case(decision(Decision::CheckSoloLimit, 0, -10, -9).into(), &[1], None, 0; "non backoff decision")]
#[test_case(inferred(Inference::Failure).into(), &[4], None, 0; "carried pass is not a failure")]
#[test_case(inferred(Inference::Failure).into(), &[1], Some(EPOCH + SignedDuration::from_secs(1)), 0; "decision before cutoff")]
#[test_case(inferred(Inference::Failure).into(), &[1], Some(EPOCH + SignedDuration::from_secs(0)), 1; "decision at cutoff uses earlier evidence")]
fn a_decision_resting_on_one_carried_failure_is_counted(
    payload: Payload,
    cause: &[u64],
    since: Option<jiff::Timestamp>,
    want: u32,
) {
    let events = pool(0, payload, cause);
    assert_eq!(single_carried_failure_decisions(&events, since), want);
}

#[test]
fn a_chain_through_hunt_events_is_followed_to_its_failures() {
    let chain = |extra: &[u64]| {
        let mut end_cause = vec![5, 2, 6];
        end_cause.extend_from_slice(extra);
        let events = vec![
            event(1, 0, &[], carried(Outcome::Failure)),
            event(2, 0, &[], carried(Outcome::Pass)),
            event(3, 0, &[], super::support::end("a", Outcome::Failure)),
            event(4, 0, &[], carried_idle_failure()),
            event(5, 0, &[1], hunt_start(1, "t", 1)),
            event(6, 0, &[3], super::support::end("b", Outcome::Pass)),
            event(
                7,
                0,
                &end_cause,
                super::support::hunt_end(1, togi_journal::HuntOutcome::Combination),
            ),
            event(
                8,
                0,
                &[7],
                togi_journal::Combination {
                    combination: nz(1),
                    members: Vec::new(),
                    fallback: false,
                    hunt: nz(1),
                    reason: String::new(),
                },
            ),
            event(9, 0, &[8], decision(Decision::Backoff, 0, -10, -9)),
        ];
        single_carried_failure_decisions(&events, Some(EPOCH))
    };
    assert_eq!(chain(&[]), 1, "a carried failure through a combination");
    assert_eq!(chain(&[3]), 0, "a live failure added");
    assert_eq!(chain(&[4]), 0, "a second carried failure added");
    assert_eq!(chain(&[1, 5]), 1, "one failure through several paths");
}

/// Classes of two R4 workloads whose catalog order differs from their alphabetical order, each at 90 s and 120 s.
const CATALOG_ORDER: [&str; 4] = [
    "R4/ycruncher-bkt-sftv4-duty50/00,01/90s",
    "R4/ycruncher-bkt-sftv4-duty50/00,01/120s",
    "R4/mprime-sse-4k-21k-duty50/00,01/90s",
    "R4/mprime-sse-4k-21k-duty50/00,01/120s",
];

#[test]
fn class_tables_list_workloads_in_catalog_order_and_durations_numerically() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    for (n, (index, duration)) in [(3, 120), (1, 120), (3, 90), (1, 90)]
        .into_iter()
        .enumerate()
    {
        for (outcome, tag) in [(Outcome::Pass, "p"), (Outcome::Failure, "f")] {
            let mut planned = intent(&format!("{tag}{n}"), Regime::R4, &[0, 1], &[-10, -10]);
            planned.workload = workload(Regime::R4, index);
            planned.duration_s = duration;
            scheduled_trial(&mut log, planned, outcome, None);
        }
    }
    let journal = log.journal();
    let p = Projection::new(&journal);
    let m = compute(&p, None);
    let contradictions: Vec<_> = m
        .evidence
        .contradictions
        .keys()
        .map(|c| c.label())
        .collect();
    assert_eq!(contradictions, CATALOG_ORDER);
    let depth: Vec<_> = m.depth.iter().map(|r| r.class.label()).collect();
    assert_eq!(depth, CATALOG_ORDER);
}

#[test]
fn r7_request_tables_list_durations_numerically() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    for (n, duration) in [120, 90].into_iter().enumerate() {
        let id = n.to_string();
        let mut planned = intent(&id, Regime::R7, &[0, 1], &[0, 0]);
        planned.duration_s = duration;
        log.add(planned);
        log.add(super::support::trial_start(&id));
        let mut ended = super::support::end(&id, Outcome::Pass);
        ended.voltage_requests_v = [(set(&[0]).iter().next().unwrap(), 1.0)]
            .into_iter()
            .collect::<PerCore<f64>>();
        ended.top_requesters = set(&[0]);
        log.add(ended);
    }
    let journal = log.journal();
    let m = crate::r7::requests(&Projection::new(&journal), None);
    let bins: Vec<_> = m.bins.keys().map(|(class, _)| class.label()).collect();
    let requesters: Vec<_> = m
        .requesters
        .keys()
        .map(|(class, _)| class.label())
        .collect();
    let want = [
        "R7/mprime-avx2-36k-248k-allcore/00,01/90s",
        "R7/mprime-avx2-36k-248k-allcore/00,01/120s",
    ];
    assert_eq!(bins, want);
    assert_eq!(requesters, want);
}

#[test_case(None, u32::MAX, None; "unknown remains unknown")]
#[test_case(Some(0), 0, Some(true); "zero requirement")]
#[test_case(Some(0), 1, Some(false); "missing evidence")]
#[test_case(Some(u32::MAX as usize - 1), u32::MAX, Some(false); "just below largest requirement")]
#[test_case(Some(u32::MAX as usize), u32::MAX, Some(true); "largest requirement")]
#[test_case(Some(usize::MAX), u32::MAX, Some(true); "largest observed count")]
fn establishment_compares_exact_counts(prior: Option<usize>, required: u32, expected: Option<bool>) {
    let group = crate::evidence::GroupReuse {
        hunt: 1,
        group: 1,
        stage: HuntStage::Part,
        prior,
        required,
        passes: 0,
        failures: 0,
        trials: 0,
        seconds: 0,
    };
    assert_eq!(group.established(), expected);
}
