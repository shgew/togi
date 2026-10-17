use jiff::SignedDuration;
use togi_journal::{
    Activity, CycleEvent, Decision, Event, FactSource, FailureCarried, HuntStage, Inference,
    Outcome, Payload, Stamp, TrialCarried, TrialClass, TrialIntent, TunerWarning, WarningKind,
};
use togi_machine::{BootId, Condition, PerCore, Regime, Signal};
use togi_tuner::Profile;

use super::support::{
    EPOCH, Log, build, cycle, decision, failure, hunt_group, hunt_start, intent, nz, profile, seq,
    set, trial, workload,
};
use crate::evidence::{prior_passes, single_carried_failure_decisions};
use crate::project::Projection;
use crate::report::compute;

fn target_intent() -> TrialIntent {
    let mut target = intent("target", Regime::R7, &[1, 0], &[-10, -10]);
    target.condition = Condition::Parked;
    target
}

type Past = (Vec<i64>, Outcome, fn(&mut TrialIntent));

struct Case {
    name: &'static str,
    history: Vec<Past>,
    want: usize,
    /// The index of the history trial whose end the query stops at, or after every trial.
    before: Option<usize>,
}

fn nothing(_: &mut TrialIntent) {}

fn prior(case: &Case) -> usize {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    let mut ends = Vec::new();
    for (i, (offsets, outcome, modify)) in case.history.iter().enumerate() {
        let mut past = target_intent();
        past.trial = format!("h{i}").into();
        past.profile = profile(offsets);
        modify(&mut past);
        ends.push(trial(&mut log, past, *outcome));
    }
    let before = case
        .before
        .map_or_else(|| seq(log.events.len() as u64 + 1), |index| ends[index]);
    let journal = log.journal();
    let p = Projection::new(&journal);
    let target = target_intent();
    prior_passes(
        &p,
        &target,
        Profile::new(p.topology, target.profile).ok(),
        before,
    )
}

#[test]
fn prior_passes_count_what_is_as_deep_and_not_ruled_out_by_a_failure() {
    let pass = Outcome::Pass;
    let cases = [
        Case {
            name: "equal and deeper count",
            history: vec![
                (vec![-10, -10], pass, nothing),
                (vec![-11, -10], pass, nothing),
            ],
            want: 2,
            before: None,
        },
        Case {
            name: "shallow and incomparable do not",
            history: vec![
                (vec![-9, -10], pass, nothing),
                (vec![-11, -9], pass, nothing),
            ],
            want: 0,
            before: None,
        },
        Case {
            name: "later shallow failure invalidates",
            history: vec![
                (vec![-11, -11], pass, nothing),
                (vec![-9, -9], Outcome::Failure, nothing),
                (vec![-10, -10], pass, nothing),
            ],
            want: 1,
            before: None,
        },
        Case {
            name: "deeper failure does not invalidate",
            history: vec![
                (vec![-10, -10], pass, nothing),
                (vec![-11, -11], Outcome::Failure, nothing),
            ],
            want: 1,
            before: None,
        },
        Case {
            name: "same cores and another condition",
            history: vec![(vec![-10, -10], pass, |t| t.condition = Condition::Together)],
            want: 1,
            before: None,
        },
        Case {
            name: "class fields matter",
            history: vec![
                (vec![-10, -10], pass, |t| t.duration_s = 300),
                (vec![-10, -10], pass, |t| {
                    t.workload = workload(Regime::R7, 1);
                }),
                (vec![-10, -10], pass, |t| {
                    t.regime = Regime::R6;
                    t.workload = workload(Regime::R6, 0);
                }),
                (vec![-10, -10], pass, |t| t.cores = set(&[0])),
            ],
            want: 0,
            before: None,
        },
        Case {
            name: "the hunt boundary is excluded",
            history: vec![
                (vec![-10, -10], pass, nothing),
                (vec![-10, -10], pass, nothing),
            ],
            want: 0,
            before: Some(0),
        },
        Case {
            name: "inconclusive does not invalidate",
            history: vec![
                (vec![-10, -10], pass, nothing),
                (vec![-9, -9], Outcome::Inconclusive, nothing),
            ],
            want: 1,
            before: None,
        },
    ];
    for case in &cases {
        assert_eq!(prior(case), case.want, "{}", case.name);
    }
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
    trial(&mut log, planned("pass", None), Outcome::Pass);
    let mut crash = failure(None);
    crash.signal = Signal::Crash;
    crash.profile = idle_profile.map(profile);
    log.add(crash);
    log.add(hunt_start(1, "t", 1));
    log.add(hunt_group(1, 1, HuntStage::Part));
    trial(&mut log, planned("group", Some(1)), Outcome::Pass);
    let journal = log.journal();
    compute(&Projection::new(&journal), None).evidence.groups[0].prior
}

#[test]
fn an_idle_crash_invalidates_the_passes_of_an_all_core_r6_target_it_is_no_deeper_than() {
    let r6 = Regime::R6;
    assert_eq!(idle_crash(r6, &[0, 1], None), 1, "no recorded profile");
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
    trial(&mut log, planned("pass", None), Outcome::Pass);
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
    assert_eq!(m.evidence.groups[0].prior, 1);
}

#[test]
fn a_failure_after_a_prior_pass_is_listed_by_class() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    trial(
        &mut log,
        intent("a", Regime::R6, &[0, 1], &[-10, -10]),
        Outcome::Pass,
    );
    trial(
        &mut log,
        intent("b", Regime::R6, &[0, 1], &[-10, -10]),
        Outcome::Failure,
    );
    trial(
        &mut log,
        intent("c", Regime::R6, &[0, 1], &[-11, -11]),
        Outcome::Failure,
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

type Scenario = (
    &'static str,
    Payload,
    &'static [u64],
    Option<jiff::Timestamp>,
    u32,
);

#[test]
fn a_decision_resting_on_one_carried_failure_is_counted() {
    let backoff = || decision(Decision::Backoff, 0, -10, -9);
    let at = |seconds| Some(EPOCH + SignedDuration::from_secs(seconds));
    let cases: [Scenario; 12] = [
        (
            "inferred failure with hunt context",
            inferred(Inference::Failure).into(),
            &[5, 1],
            None,
            1,
        ),
        (
            "carried idle failure backoff",
            backoff().into(),
            &[2],
            None,
            1,
        ),
        (
            "repeated cause is one observation",
            inferred(Inference::Failure).into(),
            &[1, 1],
            None,
            1,
        ),
        (
            "two carried failures",
            inferred(Inference::Failure).into(),
            &[1, 2],
            None,
            0,
        ),
        (
            "carried and live failures",
            backoff().into(),
            &[1, 3],
            None,
            0,
        ),
        ("live failure only", backoff().into(), &[3], None, 0),
        (
            "passing inference with context failure",
            inferred(Inference::Pass).into(),
            &[1, 4, 5],
            None,
            0,
        ),
        (
            "hunt start context citation",
            hunt_start(1, "t", 1).into(),
            &[1],
            None,
            0,
        ),
        (
            "non-backoff decision",
            decision(Decision::CheckSoloLimit, 0, -10, -9).into(),
            &[1],
            None,
            0,
        ),
        (
            "carried pass is not a failure",
            inferred(Inference::Failure).into(),
            &[4],
            None,
            0,
        ),
        (
            "decision before the cutoff",
            inferred(Inference::Failure).into(),
            &[1],
            at(1),
            0,
        ),
        (
            "decision at the cutoff uses earlier evidence",
            inferred(Inference::Failure).into(),
            &[1],
            at(0),
            1,
        ),
    ];
    for (name, payload, cause, since, want) in cases {
        let events = pool(0, payload, cause);
        assert_eq!(
            single_carried_failure_decisions(&events, since),
            want,
            "{name}"
        );
    }
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
            trial(&mut log, planned, outcome);
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
