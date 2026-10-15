use jiff::SignedDuration;
use togi_journal::{Outcome, TrialCarried, TrialClass};
use togi_machine::{Condition, PerCore, Regime};

use super::evidence::carried;
use super::support::{EPOCH, Log, end, intent, rendered, set, trial_start, workload};
use crate::project::Projection;
use crate::r7::{RequestCounts, render_requests, requests};

fn measured(requests: &[(i64, f64)]) -> PerCore<f64> {
    requests
        .iter()
        .map(|&(n, v)| (super::support::core(n), v))
        .collect()
}

struct Trial<'a> {
    regime: Regime,
    workload: usize,
    cores: &'a [i64],
    duration: u32,
    outcome: Outcome,
    top: f64,
    requesters: &'a [i64],
    started: bool,
    seconds: i64,
}

const BASE: Trial<'static> = Trial {
    regime: Regime::R7,
    workload: 0,
    cores: &[0, 1],
    duration: 120,
    outcome: Outcome::Pass,
    top: 1.096,
    requesters: &[1],
    started: true,
    seconds: 100,
};

fn add(log: &mut Log, id: usize, t: &Trial<'_>) {
    let id = id.to_string();
    let mut planned = intent(&id, t.regime, t.cores, &[0, 0]);
    planned.workload = workload(t.regime, t.workload);
    planned.duration_s = t.duration;
    log.at(t.seconds).add(planned);
    if t.started {
        log.add(trial_start(&id));
    }
    let mut ended = end(&id, t.outcome);
    if t.top != 0.0 {
        ended.voltage_requests_v = measured(&[(0, t.top - 0.01), (1, t.top)]);
    }
    ended.top_requesters = set(t.requesters);
    log.add(ended);
}

#[test]
fn r7_trials_are_counted_by_top_request_bin_and_by_top_requester() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    let trials = [
        Trial {
            cores: &[1, 0],
            ..BASE
        },
        Trial {
            outcome: Outcome::Failure,
            top: 1.0999,
            requesters: &[0, 1],
            ..BASE
        },
        Trial {
            outcome: Outcome::Inconclusive,
            top: 1.1,
            ..BASE
        },
        Trial {
            duration: 300,
            ..BASE
        },
        Trial {
            workload: 1,
            ..BASE
        },
        Trial {
            cores: &[1],
            ..BASE
        },
        Trial {
            outcome: Outcome::Failure,
            top: 0.0,
            requesters: &[],
            ..BASE
        },
        Trial {
            outcome: Outcome::Failure,
            requesters: &[],
            ..BASE
        },
        Trial {
            outcome: Outcome::Failure,
            top: 0.0,
            ..BASE
        },
        Trial {
            outcome: Outcome::Failure,
            started: false,
            ..BASE
        },
        Trial {
            regime: Regime::R1,
            outcome: Outcome::Failure,
            ..BASE
        },
        Trial {
            outcome: Outcome::Failure,
            seconds: 99,
            ..BASE
        },
    ];
    for (id, t) in trials.iter().enumerate() {
        add(&mut log, id, t);
    }
    let journal = log.journal();
    let p = Projection::new(&journal);
    let since = Some(EPOCH + SignedDuration::from_secs(100));
    assert_snapshot!(
        "requests__r7_trials_are_counted_by_top_request_bin_and_by_top_requester",
        rendered(|table| render_requests(table, &requests(&p, since)))
    );
}

#[test]
fn carried_facts_count_once_per_source_and_only_with_both_request_fields() {
    let mut log = Log::new(&[(0, 0), (1, 0)]);
    let r7_class = TrialClass {
        regime: Regime::R7,
        workload: Some(workload(Regime::R7, 0)),
        cores: set(&[1, 0]),
        duration_s: 120,
    };
    let fact = |session: &str, number: u64, outcome, telemetry: bool| {
        let mut fact: TrialCarried = carried(outcome);
        fact.source.session = session.parse().unwrap();
        fact.source.seq = super::support::seq(number);
        fact.class = r7_class.clone();
        fact.condition = Condition::Together;
        if telemetry {
            fact.voltage_requests_v = measured(&[(0, 1.1), (1, 1.2)]);
            fact.top_requesters = set(&[1]);
        }
        fact
    };
    log.add(fact("20251201T000000Z", 7, Outcome::Failure, true));
    log.add(fact("20251201T000000Z", 7, Outcome::Failure, true));
    log.add(fact("20251101T000000Z", 7, Outcome::Pass, true));
    log.add(fact("20251201T000000Z", 9, Outcome::Failure, false));
    log.add(fact(super::support::SESSION, 9, Outcome::Failure, true));
    let mut other_regime = fact("20251201T000000Z", 11, Outcome::Failure, true);
    other_regime.class = TrialClass {
        regime: Regime::R6,
        workload: Some(workload(Regime::R6, 0)),
        cores: set(&[0]),
        duration_s: 120,
    };
    log.add(other_regime);
    add(
        &mut log,
        1,
        &Trial {
            outcome: Outcome::Failure,
            top: 1.2,
            seconds: 20,
            ..BASE
        },
    );
    let journal = log.journal();
    let p = Projection::new(&journal);
    let label = "R7/mprime-avx2-36k-248k-allcore/00,01/120s".to_owned();
    let got = requests(&p, None);
    assert_eq!(got.missing_failures, 0);
    assert_eq!(
        got.bins
            .into_iter()
            .map(|((class, bin), counts)| ((class.label(), bin), counts))
            .collect::<Vec<_>>(),
        [(
            (label.clone(), 300),
            RequestCounts {
                trials: 3,
                failures: 2
            }
        )]
    );
    assert_eq!(
        got.requesters
            .into_iter()
            .map(|((class, core), counts)| ((class.label(), core), counts))
            .collect::<Vec<_>>(),
        [(
            (label, 1),
            RequestCounts {
                trials: 3,
                failures: 2
            }
        )]
    );
    let later = Some(EPOCH + SignedDuration::from_secs(1000));
    assert_eq!(requests(&p, later), crate::r7::RequestMetrics::default());
}
