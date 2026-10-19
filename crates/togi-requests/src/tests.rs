use std::collections::BTreeMap;

use test_case::test_case;
use togi_machine::{CcdId, CoreId, CoreSet, PerCore, PmTable, TrialConditions};

use super::{MIN_SAMPLES, Telemetry, VOLTS_PER_COUNT, WARMUP_MS, counts, groups, summarize, top};

fn core(id: i64) -> CoreId {
    CoreId::new(id).unwrap()
}

fn cores(ids: &[i64]) -> CoreSet {
    ids.iter().map(|&id| core(id)).collect()
}

fn placed(ids: &[i64]) -> PerCore<Option<CcdId>> {
    ids.iter()
        .map(|&id| (core(id), Some(CcdId::new(id / 8).unwrap())))
        .collect()
}

fn per_core<T: Copy + Default>(readings: &[(i64, T)]) -> PerCore<T> {
    readings
        .iter()
        .map(|&(id, value)| (core(id), value))
        .collect()
}

fn lane(value: f32) -> f64 {
    f64::from(value)
}

fn sample(elapsed_ms: i64, requests: &[(i64, f32)], mhz: &[(i64, i64)]) -> TrialConditions {
    let mut table = PmTable::default();
    for &(id, volts) in requests {
        table.voltage_request_v[core(id).index()] = volts;
    }
    TrialConditions {
        elapsed_ms,
        core_mhz: per_core(mhz),
        pm_table: Some(table),
        ..TrialConditions::default()
    }
}

fn samples(
    from: i64,
    to: i64,
    lanes: impl Fn(i64) -> (Vec<(i64, f32)>, Vec<(i64, i64)>),
) -> Vec<TrialConditions> {
    (from..=to)
        .step_by(1000)
        .map(|ms| {
            let (requests, mhz) = lanes(ms);
            sample(ms, &requests, &mhz)
        })
        .collect()
}

#[test]
fn summarize_uses_samples_after_warmup() {
    let seq = samples(1000, 24000, |ms| {
        if ms < WARMUP_MS {
            (
                vec![(0, 1.5), (1, 1.5), (8, 1.5)],
                vec![(0, 9000), (1, 9000), (8, 9000)],
            )
        } else {
            (
                vec![(0, 1.100), (1, 1.0995), (8, 1.050)],
                vec![(0, 5200), (1, 5210), (8, 5100)],
            )
        }
    });
    let got = summarize(seq, &placed(&[0, 1, 8])).expect("20 samples after the warmup");
    let want = Telemetry {
        requests: per_core(&[(0, lane(1.100)), (1, lane(1.0995)), (8, lane(1.050))]),
        top_requesters: cores(&[0, 1, 8]),
        ccd_mhz: BTreeMap::from([
            (CcdId::new(0).unwrap(), 5205),
            (CcdId::new(1).unwrap(), 5100),
        ]),
    };
    assert_eq!(got, want);
}

#[test]
fn summarize_needs_min_samples_after_warmup() {
    let steady = |_| (vec![(3, 1.1)], vec![(3, 5000)]);
    let last = |count: usize| WARMUP_MS + (i64::try_from(count).unwrap() - 1) * 1000;
    assert!(
        summarize(samples(0, last(MIN_SAMPLES - 1), steady), &placed(&[3])).is_none(),
        "19 samples after the warmup summarized"
    );
    assert!(
        summarize(samples(0, last(MIN_SAMPLES), steady), &placed(&[3])).is_some(),
        "20 samples after the warmup did not summarize"
    );
}

#[test]
fn summarize_needs_min_clock_samples_per_ccd() {
    let seq = samples(
        WARMUP_MS,
        WARMUP_MS + (i64::try_from(MIN_SAMPLES).unwrap() - 1) * 1000,
        |ms| {
            let mhz = if ms == WARMUP_MS {
                vec![(0, 5200)]
            } else {
                vec![(0, 5200), (8, 5100)]
            };
            (vec![(0, 1.1), (8, 1.05)], mhz)
        },
    );
    let got = summarize(seq, &placed(&[0, 8])).expect("20 samples after the warmup");
    let want = Telemetry {
        requests: per_core(&[(0, lane(1.1)), (8, lane(1.05))]),
        top_requesters: cores(&[0, 8]),
        ccd_mhz: BTreeMap::from([(CcdId::new(0).unwrap(), 5200)]),
    };
    assert_eq!(got, want);
}

#[test]
fn summarize_skips_samples_without_lanes() {
    let seq = (0..60).map(|index| TrialConditions {
        elapsed_ms: WARMUP_MS + index * 1000,
        core_mhz: per_core(&[(2, 5000)]),
        ..TrialConditions::default()
    });
    assert!(summarize(seq, &placed(&[2])).is_none());
}

#[test]
fn summarize_without_cores_is_none() {
    let seq = samples(WARMUP_MS, 30000, |_| (vec![(0, 1.1)], vec![(0, 5000)]));
    assert!(summarize(seq, &PerCore::default()).is_none());
}

#[test]
fn summarize_ignores_cleared_sparse_core_entries() {
    let seq = samples(WARMUP_MS, 30000, |_| (vec![(3, 1.1)], vec![(3, 5000)]));
    let mut cores = placed(&[3]);
    let expected = summarize(seq.clone(), &cores);
    cores.set(core(15), None);
    assert_eq!(summarize(seq, &cores), expected);
    cores.set(core(3), None);
    assert_eq!(summarize([], &cores), None);
}

#[test]
fn summarize_counts_a_repeated_core_once() {
    let seq = samples(WARMUP_MS, 30000, |ms| {
        let step = f32::from(u16::try_from(ms / 1000).unwrap());
        (vec![(0, 1.0 + step * 0.01)], vec![(0, 5000 + ms / 1000)])
    });
    let once = summarize(seq.clone(), &placed(&[0])).unwrap();
    let twice = summarize(seq, &placed(&[0, 0])).unwrap();
    assert_eq!(placed(&[0, 0]).len(), 1);
    assert_eq!(once, twice);
    assert_eq!(
        once.ccd_mhz,
        BTreeMap::from([(CcdId::new(0).unwrap(), 5018)])
    );
}

#[test_case(per_core(&[(1, 1.10), (2, 1.12), (3, 1.11)]), &[&[2], &[3], &[1]]; "strict order")]
#[test_case(per_core(&[(1, 1.1000), (2, 1.1009), (3, 1.0950)]), &[&[1, 2], &[3]]; "within the tie joins the top")]
#[test_case(per_core(&[(1, 1.1000), (2, 1.0992), (3, 1.0984)]), &[&[1, 2], &[3]]; "ties measure from the group's highest")]
#[test_case(per_core(&[(5, 1.1), (4, 1.1)]), &[&[4, 5]]; "equal requests group by core")]
#[test_case(PerCore::default(), &[]; "no requests")]
fn groups_orders_by_request(requests: PerCore<f64>, want: &[&[i64]]) {
    let want: Vec<CoreSet> = want.iter().map(|ids| cores(ids)).collect();
    assert_eq!(groups(&requests), want);
}

#[test_case(1.100, 1.100, 1; "target equal to the request")]
#[test_case(1.100, 1.050, 1; "target below the request")]
#[test_case(1.100, 1.100 + VOLTS_PER_COUNT, 1; "target one count above")]
#[test_case(1.100, 1.100 + VOLTS_PER_COUNT + 0.0001, 2; "target just over one count above")]
#[test_case(1.0322, 1.0443, 4; "several counts above")]
fn counts_raises_a_request_by_whole_counts(v: f64, target: f64, want: i64) {
    assert_eq!(counts(v, target), want);
}

#[test]
fn top_is_the_highest_request() {
    assert_eq!(top(&PerCore::default()), None);
    assert_eq!(top(&per_core(&[(3, 1.2), (1, 1.4), (7, 1.3)])), Some(1.4));
    assert_eq!(top(&per_core(&[(0, 1.4), (1, 1.4)])), Some(1.4));
}

#[test]
fn summarize_skips_samples_with_a_non_finite_lane() {
    let valid = |ms: i64| sample(ms, &[(0, 1.1), (1, 1.2)], &[(0, 5000), (1, 5000)]);
    let broken = |ms: i64, bad: f32| sample(ms, &[(0, 1.1), (1, bad)], &[(0, 5000), (1, 5000)]);
    let finite_only = |count: i64| (0..count).map(|index| valid(WARMUP_MS + index * 1000));
    let noisy = |count: i64| {
        finite_only(count)
            .chain([f32::NAN, f32::INFINITY, f32::NEG_INFINITY].map(|bad| broken(WARMUP_MS, bad)))
            .collect::<Vec<_>>()
    };
    let min = i64::try_from(MIN_SAMPLES).unwrap();
    assert!(summarize(noisy(min - 1), &placed(&[0, 1])).is_none());
    let got = summarize(noisy(min), &placed(&[0, 1])).unwrap();
    assert_eq!(got, summarize(finite_only(min), &placed(&[0, 1])).unwrap());
    assert!(got.requests.iter().all(|(_, request)| request.is_finite()));
    let unloaded = broken(WARMUP_MS, f32::NAN);
    assert!(summarize(finite_only(min).chain([unloaded]), &placed(&[0])).is_some());
}
