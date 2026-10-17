use std::collections::BTreeMap;
use std::num::{NonZeroU16, NonZeroU32};

use proptest::array::uniform4;
use proptest::prelude::*;
use togi_journal::Seq;
use togi_machine::WorkloadId;
use togi_machine::{CcdId, Condition, CoreId, CoreInfo, CoreSet, PerCore, Regime, TargetOffset};
use togi_requests::{VOLTS_PER_COUNT, top};

use super::Applied;
use super::escalation::{
    BackoffKind, ESCALATE_AFTER, Escalation, Strikes, loaded_stuck_at_zero, unloaded_off_zero,
};
use super::measurements::{Measurement, Measurements};
use super::order::{Lane, RequestOrder, lowest_ranked};
use super::status::SelfSufficiency;
use super::target::{
    Ask, OwnRequest, PassRecord, R7Targets, Target, backoff_counts, backoff_move, counts_above,
};
use crate::tests::{core, off};
use crate::{
    BackendId, ClassKey, Depth, Evidence, LoadKey, Observation, Profile, Ranking, Secs, Topology,
};

const TOL: f64 = 1e-9;

fn seq(n: u64) -> Seq {
    Seq::new(n).unwrap()
}

fn n_of(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap()
}

fn set(ids: &[u8]) -> CoreSet {
    ids.iter().map(|&id| core(id)).collect()
}

fn workload(index: usize) -> WorkloadId {
    WorkloadId::new(Regime::R7, index).unwrap()
}

fn load(index: usize, ids: &[u8]) -> LoadKey {
    LoadKey::new(workload(index), set(ids))
}

fn class(index: usize, ids: &[u8], duration: u32) -> ClassKey {
    ClassKey::new(
        workload(index),
        set(ids),
        Secs::new(NonZeroU32::new(duration).unwrap()),
    )
}

/// Cores 0 and 1 on CCD 0, cores 2 and 3 on CCD 1.
fn two_ccds() -> Topology {
    let infos: Vec<CoreInfo> = (0..4_u8)
        .map(|id| CoreInfo {
            core: core(id),
            ccd: CcdId(id / 2),
            cpus: Vec::new(),
        })
        .collect();
    Topology::new(&infos).unwrap()
}

fn profile_of(topology: &Topology, values: &[i64]) -> Profile {
    crate::tests::profile(topology, values)
}

fn requests(lanes: &[(u8, f64)]) -> PerCore<f64> {
    lanes.iter().map(|&(id, v)| (core(id), v)).collect()
}

fn measurement(at: u64, profile: Profile, lanes: &[(u8, f64)]) -> Measurement {
    Measurement::new(seq(at), profile, requests(lanes))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < TOL
}

fn backend() -> BackendId {
    Evidence::new(CoreSet::default()).active_backend(togi_machine::Backend::Mprime)
}

fn volts(order: &RequestOrder, id: u8) -> f64 {
    order.volts(core(id)).unwrap()
}

fn groups_of(order: &RequestOrder, ccd: u8) -> Vec<Vec<u8>> {
    order
        .ccd(CcdId(ccd))
        .unwrap()
        .groups()
        .iter()
        .map(|group| group.iter().map(CoreId::get).collect())
        .collect()
}

mod order {
    use super::*;

    #[test]
    fn shifts_the_ccd_full_part_to_the_profile_and_restricts_it() {
        let topo = two_ccds();
        let mut store = Measurements::default();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        store.record(load(0, &[0, 1]), measurement(5, at, &[(0, 1.08), (1, 1.1)]));
        let target = profile_of(&topo, &[-20, -30, -30, -30]);
        let order = RequestOrder::derive(&store, &topo, load(0, &[0]), &target);
        assert!(close(volts(&order, 0), 1.08 + VOLTS_PER_COUNT * 10.0));
        assert!(close(volts(&order, 0), 1.116));
        assert_eq!(order.sources(), [seq(5)]);
        assert_eq!(order.lane(core(0)), Some(Lane::Measured { source: seq(5) }));
        assert_eq!(order.lane(core(1)), None);
    }

    #[test]
    fn falls_back_to_offsets_without_a_measurement_of_the_workload() {
        let topo = two_ccds();
        let mut store = Measurements::default();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        store.record(load(0, &[0, 1]), measurement(5, at, &[(0, 1.08), (1, 1.1)]));
        let order = RequestOrder::derive(&store, &topo, load(1, &[0, 1]), &at);
        assert!(order.is_offset_only());
        assert!(order.sources().is_empty());
        assert_eq!(order.proxy(), set(&[0, 1]));
        assert_eq!(order.volts(core(0)), None);
        assert_eq!(groups_of(&order, 0), [[0, 1]]);
        assert_eq!(order.top(), set(&[0, 1]));
    }

    #[test]
    fn offset_proxy_orders_shallowest_first_and_ties_equal_offsets() {
        let topo = two_ccds();
        let store = Measurements::default();
        let shallow = profile_of(&topo, &[-10, -30, -30, -30]);
        let order = RequestOrder::derive(&store, &topo, load(0, &[0, 1]), &shallow);
        assert_eq!(groups_of(&order, 0), [vec![0], vec![1]]);
        assert_eq!(order.top(), set(&[0]));
        let equal = profile_of(&topo, &[-30, -30, -30, -30]);
        let order = RequestOrder::derive(&store, &topo, load(0, &[0, 1]), &equal);
        assert_eq!(groups_of(&order, 0), [[0, 1]]);
    }

    #[test]
    fn exact_load_beats_the_ccd_part_even_when_older() {
        let topo = two_ccds();
        let mut store = Measurements::default();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        store.record(load(0, &[0, 1]), measurement(9, at, &[(0, 1.2), (1, 1.1)]));
        store.record(load(0, &[0]), measurement(3, at, &[(0, 1.05)]));
        let order = RequestOrder::derive(&store, &topo, load(0, &[0]), &at);
        assert!(close(volts(&order, 0), 1.05));
        assert_eq!(order.sources(), [seq(3)]);
    }

    #[test]
    fn duration_and_outcome_do_not_split_a_load() {
        let mut store = Measurements::default();
        let topo = two_ccds();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        let key = load(0, &[0, 1]);
        assert!(store.record(key, measurement(1, at, &[(0, 1.0), (1, 1.0)])));
        assert!(store.record(key, measurement(2, at, &[(0, 1.1), (1, 1.1)])));
        assert_eq!(store.newest(key).unwrap().seq(), seq(2));
    }

    #[test]
    fn an_older_measurement_arriving_late_does_not_replace_the_newest() {
        let mut store = Measurements::default();
        let topo = two_ccds();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        let key = load(0, &[0, 1]);
        assert!(store.record(key, measurement(7, at, &[(0, 1.1)])));
        assert!(!store.record(key, measurement(4, at, &[(0, 1.3)])));
        assert!(!store.record(key, measurement(7, at, &[(0, 1.3)])));
        assert!(close(
            store.newest(key).unwrap().request(core(0)).unwrap(),
            1.1
        ));
    }

    #[test]
    fn a_measurement_without_a_lane_is_no_measurement() {
        let mut store = Measurements::default();
        let topo = two_ccds();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        assert!(!store.record(load(0, &[0, 1]), measurement(1, at, &[])));
        assert!(store.newest(load(0, &[0, 1])).is_none());
    }

    #[test]
    fn a_missing_lane_is_ordered_by_offset_never_read_as_zero_volts() {
        let topo = two_ccds();
        let mut store = Measurements::default();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        store.record(load(0, &[0, 1]), measurement(2, at, &[(0, 1.1)]));
        let order = RequestOrder::derive(&store, &topo, load(0, &[0, 1]), &at);
        assert_eq!(order.lane(core(1)), Some(Lane::OffsetProxy));
        assert_eq!(order.volts(core(1)), None);
        assert_eq!(order.proxy(), set(&[1]));
        assert_eq!(order.sources(), [seq(2)]);
        assert_eq!(groups_of(&order, 0), [vec![0], vec![1]]);
    }

    #[test]
    fn an_all_core_load_has_a_top_group_on_each_ccd() {
        let topo = two_ccds();
        let mut store = Measurements::default();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        store.record(
            load(0, &[0, 1, 2, 3]),
            measurement(1, at, &[(0, 1.1), (1, 1.08), (2, 1.2), (3, 1.19)]),
        );
        let order = RequestOrder::derive(&store, &topo, load(0, &[0, 1, 2, 3]), &at);
        assert_eq!(groups_of(&order, 0), [vec![0], vec![1]]);
        assert_eq!(groups_of(&order, 1), [vec![2], vec![3]]);
        assert_eq!(order.top(), set(&[0, 2]));
        let ccds: Vec<(CcdId, CoreSet)> = order
            .ccds()
            .iter()
            .map(|ccd| (ccd.ccd(), ccd.loaded()))
            .collect();
        assert_eq!(ccds, [(CcdId(0), set(&[0, 1])), (CcdId(1), set(&[2, 3]))]);
        assert!(close(order.ccd(CcdId(0)).unwrap().rail(), 1.1));
        assert!(close(order.ccd(CcdId(1)).unwrap().rail(), 1.2));
    }

    #[test]
    fn a_measured_ccd_beside_an_unmeasured_one_mixes_lanes_per_ccd() {
        let topo = two_ccds();
        let mut store = Measurements::default();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        store.record(load(0, &[0, 1]), measurement(1, at, &[(0, 1.2), (1, 1.15)]));
        let order = RequestOrder::derive(&store, &topo, load(0, &[0, 1, 2, 3]), &at);
        assert_eq!(order.sources(), [seq(1)]);
        assert_eq!(order.proxy(), set(&[2, 3]));
        assert_eq!(order.volts(core(3)), None);
        assert_eq!(groups_of(&order, 1), [[2, 3]]);
    }

    #[test]
    fn the_trials_own_measurement_takes_precedence() {
        let topo = two_ccds();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        let own = measurement(8, at, &[(0, 1.1), (1, 1.2)]);
        let order = RequestOrder::measured(&topo, load(0, &[0, 1]), &own);
        assert_eq!(order.sources(), [seq(8)]);
        assert_eq!(groups_of(&order, 0), [vec![1], vec![0]]);
        assert!(close(volts(&order, 1), 1.2));
    }

    #[test]
    fn a_frozen_order_ignores_measurements_that_arrive_later() {
        let topo = two_ccds();
        let mut store = Measurements::default();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        let key = load(0, &[0, 1]);
        store.record(key, measurement(1, at, &[(0, 1.2), (1, 1.1)]));
        let frozen = RequestOrder::derive(&store, &topo, key, &at);
        store.record(key, measurement(3, at, &[(0, 1.1), (1, 1.2)]));
        let later = RequestOrder::derive(&store, &topo, key, &at);
        assert_eq!(frozen.top(), set(&[0]));
        assert_eq!(frozen.sources(), [seq(1)]);
        assert_eq!(later.top(), set(&[1]));
        assert_eq!(later.sources(), [seq(3)]);
    }

    #[test]
    fn groups_use_the_highest_anchor_inclusive_one_millivolt_and_are_not_transitive() {
        let topo = two_ccds();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        let own = measurement(1, at, &[(0, 1.2), (1, 1.1995), (2, 1.1985), (3, 1.0)]);
        let order = RequestOrder::measured(&topo, load(0, &[0, 1]), &own);
        assert_eq!(groups_of(&order, 0), [[0, 1]]);
        let own = measurement(1, at, &[(0, 1.2), (1, 1.1995)]);
        let order = RequestOrder::measured(&topo, load(0, &[0, 1]), &own);
        assert_eq!(groups_of(&order, 0), [[0, 1]]);
        let wide = Topology::new(
            &(0..3_u8)
                .map(|id| CoreInfo {
                    core: core(id),
                    ccd: CcdId(0),
                    cpus: Vec::new(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let at = profile_of(&wide, &[-30, -30, -30]);
        let own = measurement(1, at, &[(0, 1.2), (1, 1.1995), (2, 1.1985)]);
        let order = RequestOrder::measured(&wide, load(0, &[0, 1, 2]), &own);
        assert_eq!(groups_of(&order, 0), [vec![0, 1], vec![2]]);
    }

    #[test]
    fn reset_drops_the_measurements_whose_load_includes_the_core_only() {
        let topo = two_ccds();
        let mut store = Measurements::default();
        let at = profile_of(&topo, &[-30, -30, -30, -30]);
        store.record(load(0, &[0, 1]), measurement(1, at, &[(0, 1.1), (1, 1.1)]));
        store.record(load(0, &[2, 3]), measurement(2, at, &[(2, 1.1), (3, 1.1)]));
        store.reset_core(core(3));
        assert!(store.newest(load(0, &[0, 1])).is_some());
        assert!(store.newest(load(0, &[2, 3])).is_none());
    }

    #[test]
    fn first_movable_steps_down_past_a_group_at_zero() {
        let topo = two_ccds();
        let at = profile_of(&topo, &[0, -30, -30, -30]);
        let own = measurement(1, at, &[(0, 1.2), (1, 1.15)]);
        let order = RequestOrder::measured(&topo, load(0, &[0, 1]), &own);
        let ccd = order.ccd(CcdId(0)).unwrap();
        let movable = ccd.first_movable(&at).unwrap();
        assert_eq!(movable.cores, set(&[1]));
        assert_eq!(movable.skipped, [set(&[0])]);
        assert!(movable.stepped_down());
        assert!(close(movable.rail, 1.2));
    }

    #[test]
    fn first_movable_treats_a_partly_zero_tie_as_no_step_down() {
        let topo = two_ccds();
        let at = profile_of(&topo, &[0, -30, -30, -30]);
        let own = measurement(1, at, &[(0, 1.2), (1, 1.1995)]);
        let order = RequestOrder::measured(&topo, load(0, &[0, 1]), &own);
        let movable = order.ccd(CcdId(0)).unwrap().first_movable(&at).unwrap();
        assert_eq!(movable.cores, set(&[1]));
        assert!(!movable.stepped_down());
    }

    #[test]
    fn first_movable_is_none_when_every_loaded_core_is_at_zero() {
        let topo = two_ccds();
        let at = profile_of(&topo, &[0, 0, -30, -30]);
        let order = RequestOrder::derive(&Measurements::default(), &topo, load(0, &[0, 1]), &at);
        assert!(order.ccd(CcdId(0)).unwrap().first_movable(&at).is_none());
    }

    #[test]
    fn the_less_preferred_core_moves_and_ties_go_to_the_larger_id() {
        let topo = two_ccds();
        let ranking = Ranking::new(&topo, vec![core(0), core(1), core(2), core(3)]).unwrap();
        assert_eq!(lowest_ranked(set(&[0, 1]), &ranking), Some(core(1)));
        let reversed = Ranking::new(&topo, vec![core(1), core(0), core(3), core(2)]).unwrap();
        assert_eq!(lowest_ranked(set(&[0, 1]), &reversed), Some(core(0)));
        assert_eq!(lowest_ranked(CoreSet::default(), &ranking), None);
    }
}

fn record(
    at: u64,
    class: ClassKey,
    profile: Profile,
    lanes: &[(u8, f64)],
    mhz: &[(u8, i64)],
) -> PassRecord {
    PassRecord {
        seq: seq(at),
        class,
        backend: backend(),
        profile,
        requests: requests(lanes),
        ccd_mhz: mhz.iter().map(|&(ccd, m)| (CcdId(ccd), m)).collect(),
    }
}

struct Fx {
    topo: Topology,
    evidence: Evidence,
    targets: R7Targets,
    n: NonZeroU16,
}

impl Fx {
    fn new(n: u16) -> Self {
        let topo = two_ccds();
        Self {
            evidence: Evidence::new(topo.all()),
            targets: R7Targets::default(),
            topo,
            n: n_of(n),
        }
    }

    fn profile(&self, values: &[i64]) -> Profile {
        profile_of(&self.topo, values)
    }

    fn pass(&mut self, pass: &PassRecord) -> bool {
        self.evidence
            .record_pass(
                Observation {
                    seq: pass.seq,
                    class: pass.class,
                    profile: &pass.profile,
                    condition: Condition::Together,
                    named: None,
                },
                pass.backend,
            )
            .unwrap();
        self.targets.insert(&self.topo, &self.evidence, pass)
    }

    fn fail(&mut self, at: u64, class: ClassKey, profile: Profile, named: Option<u8>) {
        self.evidence
            .record_failure(Observation {
                seq: seq(at),
                class,
                profile: &profile,
                condition: Condition::Together,
                named: named.map(core),
            })
            .unwrap();
    }

    fn reset(&mut self, id: u8) {
        self.evidence.reset_core(core(id));
        self.targets.reset_core(core(id));
    }

    fn ask(&self, backend: BackendId) -> Ask<'_> {
        Ask {
            topology: &self.topo,
            evidence: &self.evidence,
            backend,
            n: self.n,
        }
    }

    fn unattributed(&self, load: LoadKey, moved: u8, failing_top: f64) -> Option<Target> {
        self.targets
            .unattributed(self.ask(backend()), load, core(moved), failing_top)
    }

    fn named(&self, workload: WorkloadId, id: u8, mhz: Option<i64>) -> Option<Target> {
        self.targets
            .named(self.ask(backend()), workload, core(id), mhz)
    }
}

#[test]
fn named_targets_do_not_pool_loads_or_profiles_but_do_pool_durations() {
    let mut fx = Fx::new(2);
    let p = fx.profile(&[-20, -30, -30, -30]);
    fx.pass(&record(
        1,
        class(0, &[0, 1], 120),
        p,
        &[(0, 1.14)],
        &[(0, 5000)],
    ));
    fx.pass(&record(
        2,
        class(0, &[0], 120),
        p,
        &[(0, 1.13)],
        &[(0, 5000)],
    ));
    assert_eq!(fx.named(workload(0), 0, Some(5000)), None);
    let other = fx.profile(&[-21, -30, -30, -30]);
    fx.pass(&record(
        3,
        class(0, &[0, 1], 300),
        other,
        &[(0, 1.12)],
        &[(0, 5000)],
    ));
    assert_eq!(fx.named(workload(0), 0, Some(5000)), None);
    fx.pass(&record(
        4,
        class(0, &[0, 1], 300),
        p,
        &[(0, 1.15)],
        &[(0, 5000)],
    ));
    assert_eq!(
        fx.named(workload(0), 0, Some(5000)).unwrap().cited,
        vec![seq(1), seq(4)]
    );
    fx.fail(5, class(0, &[0, 1], 120), p, None);
    assert_eq!(fx.named(workload(0), 0, Some(5000)), None);
}

#[test]
fn named_target_clock_tolerance_has_an_inclusive_25_mhz_boundary() {
    for (gap, admitted) in [(0, true), (2, true), (23, true), (25, true), (26, false)] {
        let mut fx = Fx::new(1);
        let p = fx.profile(&[-20, -30, -30, -30]);
        fx.pass(&record(
            1,
            class(0, &[0, 1], 120),
            p,
            &[(0, 1.14)],
            &[(0, 5397 - gap)],
        ));
        assert_eq!(fx.named(workload(0), 0, Some(5397)).is_some(), admitted);
    }
}

#[test]
fn reconstructed_0106_to_0119_uses_the_recorded_ccd1_target() {
    // Decision-input reconstruction of session 20261010T230611Z: carried seqs 929–934,
    // and failures 0106–0119. The Go wire journal cannot be decoded by the Rust reader.
    let infos: Vec<CoreInfo> = (0..16_u8)
        .map(|id| CoreInfo {
            core: core(id),
            ccd: CcdId(id / 8),
            cpus: Vec::new(),
        })
        .collect();
    let topo = Topology::new(&infos).unwrap();
    let mut fx = Fx {
        evidence: Evidence::new(topo.all()),
        topo,
        targets: R7Targets::default(),
        n: n_of(5),
    };
    let p = fx.profile(&[
        -22, -25, -26, -34, -27, -36, -45, -42, -48, -39, -37, -42, -49, -41, -50, -50,
    ]);
    let ccd1 = [8, 9, 10, 11, 12, 13, 14, 15];
    for (at, duration, mhz, top, own) in [
        (
            929,
            120,
            5029,
            1.047_069_668_769_836_4,
            1.028_604_388_236_999_5,
        ),
        (
            930,
            120,
            5029,
            1.046_853_303_909_301_8,
            1.028_339_743_614_196_8,
        ),
        (931, 120, 5028, 1.047_316_312_789_917, 1.028_660_774_230_957),
        (
            932,
            120,
            5029,
            1.047_187_328_338_623,
            1.028_710_246_086_120_6,
        ),
        (
            933,
            120,
            5026,
            1.046_082_615_852_356,
            1.027_477_264_404_296_9,
        ),
        (
            934,
            300,
            5024,
            1.046_408_414_840_698_2,
            1.028_059_482_574_463,
        ),
    ] {
        fx.pass(&record(
            at,
            class(0, &ccd1, duration),
            p,
            &[(10, top), (11, own)],
            &[(1, mhz)],
        ));
    }
    for (trial, clock) in [
        ("0106", 5040),
        ("0107", 5040),
        ("0108", 5040),
        ("0109", 5036),
        ("0111", 5036),
        ("0115", 5017),
        ("0119", 5034),
    ] {
        let target = fx.named(workload(0), 11, Some(clock)).unwrap();
        assert_eq!(target.volts, 1.046_082_615_852_356, "{trial}");
        assert_eq!(
            target.cited,
            vec![seq(929), seq(930), seq(931), seq(932), seq(933)]
        );
    }
    let target = fx.named(workload(0), 11, Some(5040)).unwrap();
    assert_eq!(
        backoff_counts(
            OwnRequest::Measured(1.031_548_738_479_614_3),
            Some(&target),
            None
        ),
        5
    );
    let ids: Vec<u8> = (0..16).collect();
    let failing_profile = [
        -21, -24, -25, -32, -26, -35, -44, -40, -47, -38, -36, -41, -48, -40, -49, -49,
    ];
    let starts: Vec<_> = failing_profile
        .iter()
        .map(|&offset| crate::testkit::Start::has_room(offset))
        .collect();
    let mut journal = crate::testkit::Journal::on(&ids, infos, &starts);
    journal.profile_change(&failing_profile);
    let mut sources = Vec::new();
    for (top, own, mhz) in [
        (1.047_069_668_769_836_4, 1.028_604_388_236_999_5, 5029),
        (1.046_853_303_909_301_8, 1.028_339_743_614_196_8, 5029),
        (1.047_316_312_789_917, 1.028_660_774_230_957, 5028),
        (1.047_187_328_338_623, 1.028_710_246_086_120_6, 5029),
        (1.046_082_615_852_356, 1.027_477_264_404_296_9, 5026),
    ] {
        sources.push(
            journal.r7_fact(
                true,
                &ccd1,
                &[
                    -22, -25, -26, -34, -27, -36, -45, -42, -48, -39, -37, -42, -49, -41, -50, -50,
                ],
                &crate::testkit::R7Report::default()
                    .requests(&[(10, top), (11, own)])
                    .top(&[10])
                    .clocks(&[(1, mhz)]),
            ),
        );
    }
    // Install the recorded first CCD1 partial in the cycle before folding its live failure.
    // The default cycle contains the full CCD, not an arbitrary loaded subset.
    journal.open_default_cycle();
    let (step, _) = journal
        .run()
        .checking
        .locate(class(0, &ccd1, 120), None)
        .unwrap();
    journal.append(
        togi_journal::CheckingChain {
            cycle: journal.run().checking.open().unwrap().number,
            step: NonZeroU32::new(u32::try_from(step).unwrap() + 1).unwrap(),
            ccd: CcdId(1),
            workload: workload(0),
            groups: vec![set(&[10])],
            cores: set(&[8, 9, 11, 12, 13, 14, 15]),
            source_seqs: sources.clone(),
            profile: journal.profile(&failing_profile).offsets(),
            part: NonZeroU32::MIN,
            rederived: None,
        }
        .into(),
        &sources,
    );
    journal.r7_failed(
        &[8, 9, 11, 12, 13, 14, 15],
        &failing_profile,
        &crate::testkit::R7Report::default()
            .requests(&[
                (11, 1.031_548_738_479_614_3),
                (12, 1.005_218_982_696_533_2),
                (13, 1.035_337_686_538_696_3),
                (14, 1.015_336_632_728_576_7),
                (15, 1.020_572_423_934_936_5),
                (8, 1.000_017_166_137_695_3),
                (9, 1.037_670_969_963_073_7),
            ])
            .top(&[9])
            .named(11)
            .clocks(&[(1, 5040)]),
    );
    let Some(super::respond::R7Response::Backoff { decision, why }) = journal.run().r7_decision()
    else {
        panic!("expected reconstructed 0106 backoff");
    };
    assert_eq!(why.counts, 3);
    let crate::DecisionPayload::TunerDecision(td) = decision.payload else {
        panic!("expected tuner decision");
    };
    assert_eq!((td.core, td.to_offset.get()), (core(11), -38));
    journal.assert_resumes();
}

#[test]
fn latest_load_clocks_survive_empty_reports_and_backend_changes_but_not_reset() {
    let mut store = Measurements::default();
    let other = load(0, &[2, 3]);
    let load = load(0, &[0, 1]);
    let mut evidence = Evidence::new(two_ccds().all());
    let backend = evidence.active_backend(togi_machine::Backend::Mprime);
    store.record_clocks(load, backend, seq(1), &[(CcdId(0), 5000)].into());
    store.record_clocks(load, backend, seq(2), &BTreeMap::default());
    store.record_clocks(load, backend, seq(3), &[(CcdId(0), 5100)].into());
    store.record_clocks(load, backend, seq(2), &[(CcdId(0), 4900)].into());
    assert_eq!(
        store.clocks(load, backend).collect::<Vec<_>>(),
        vec![(CcdId(0), seq(3), 5100)]
    );
    assert_eq!(
        store.clocks(super::tests::load(0, &[0]), backend).count(),
        0
    );
    evidence.set_backend(
        togi_machine::Backend::Mprime,
        Some(std::path::Path::new("/packages/new-mprime")),
    );
    let changed = evidence.active_backend(togi_machine::Backend::Mprime);
    assert_ne!(changed, backend);
    assert_eq!(store.clocks(load, changed).count(), 0);
    // Interleaved reports keep one newest clock per CCD under each backend, and a CCD the
    // backend never reported stays missing.
    store.record_clocks(load, changed, seq(4), &[(CcdId(1), 4800)].into());
    store.record_clocks(load, backend, seq(5), &[(CcdId(1), 4900)].into());
    store.record_clocks(load, changed, seq(6), &[(CcdId(0), 5200)].into());
    assert_eq!(
        store.clocks(load, changed).collect::<Vec<_>>(),
        vec![(CcdId(0), seq(6), 5200), (CcdId(1), seq(4), 4800)]
    );
    evidence.set_backend(togi_machine::Backend::Mprime, None);
    assert_eq!(
        evidence.active_backend(togi_machine::Backend::Mprime),
        backend
    );
    assert_eq!(
        store.clocks(load, backend).collect::<Vec<_>>(),
        vec![(CcdId(0), seq(3), 5100), (CcdId(1), seq(5), 4900)]
    );
    store.record_clocks(other, backend, seq(7), &[(CcdId(0), 5300)].into());
    store.record_clocks(other, changed, seq(8), &[(CcdId(0), 5400)].into());
    store.reset_core(core(1));
    assert_eq!(store.clocks(load, backend).count(), 0);
    assert_eq!(store.clocks(load, changed).count(), 0);
    assert_eq!(store.clocks(other, backend).count(), 1);
    assert_eq!(store.clocks(other, changed).count(), 1);
}

#[test]
fn named_allcore_on_one_ccd_uses_that_ccds_top_and_current_backend() {
    let infos: Vec<CoreInfo> = (0..4_u8)
        .map(|id| CoreInfo {
            core: core(id),
            ccd: CcdId(0),
            cpus: Vec::new(),
        })
        .collect();
    let topo = Topology::new(&infos).unwrap();
    let mut fx = Fx {
        evidence: Evidence::new(topo.all()),
        topo,
        targets: R7Targets::default(),
        n: n_of(1),
    };
    let p = fx.profile(&[-20, -30, -30, -30]);
    fx.pass(&record(
        1,
        class(0, &[0, 1, 2, 3], 120),
        p,
        &[(0, 1.1), (1, 1.14), (2, 1.12), (3, 1.11)],
        &[(0, 5000)],
    ));
    assert_eq!(fx.named(workload(0), 0, Some(5000)).unwrap().volts, 1.14);
    fx.evidence.set_backend(
        togi_machine::Backend::Mprime,
        Some(std::path::Path::new("/packages/new-mprime")),
    );
    let active = fx.evidence.active_backend(togi_machine::Backend::Mprime);
    assert_eq!(
        fx.targets
            .named(fx.ask(active), workload(0), core(0), Some(5000)),
        None
    );
}

mod target {
    use super::*;

    fn move_to(
        applied: i64,
        prior: Option<i64>,
        own: OwnRequest,
        target: Option<&Target>,
        rail: Option<f64>,
    ) -> i64 {
        let counts = backoff_counts(own, target, rail);
        backoff_move(off(applied), prior.map(off), counts).to.get()
    }

    #[test]
    fn an_unattributed_target_rounds_up_to_whole_counts() {
        let mut fx = Fx::new(3);
        let p = fx.profile(&[-26, -30, -30, -30]);
        for at in 1..=3 {
            fx.pass(&record(
                at,
                class(0, &[0, 1], 120),
                p,
                &[(0, 1.115), (1, 1.08)],
                &[],
            ));
        }
        let target = fx.unattributed(load(0, &[0, 1]), 0, 1.1).unwrap();
        assert!(close(target.volts, 1.115));
        assert_eq!(target.cited, [seq(1), seq(2), seq(3)]);
        assert_eq!(
            move_to(-30, None, OwnRequest::Measured(1.1), Some(&target), None),
            -25
        );
    }

    #[test]
    fn an_unattributed_target_must_strictly_exceed_the_failing_top() {
        let mut fx = Fx::new(1);
        let p = fx.profile(&[-26, -30, -30, -30]);
        fx.pass(&record(
            1,
            class(0, &[0, 1], 120),
            p,
            &[(0, 1.1), (1, 1.08)],
            &[],
        ));
        let query = |failing_top| fx.unattributed(load(0, &[0, 1]), 0, failing_top);
        assert!(query(1.1).is_none());
        assert!(query(1.0999).is_some());
    }

    #[test]
    fn an_unattributed_target_uses_only_the_exact_load_and_the_moved_cores_ccd() {
        let mut fx = Fx::new(1);
        let p = fx.profile(&[-26, -30, -30, -30]);
        fx.pass(&record(
            1,
            class(0, &[0, 1, 2, 3], 120),
            p,
            &[(0, 1.12), (1, 1.1), (2, 1.3), (3, 1.2)],
            &[],
        ));
        assert!(fx.unattributed(load(0, &[0, 1]), 0, 1.0).is_none());
        let all = fx.unattributed(load(0, &[0, 1, 2, 3]), 0, 1.0).unwrap();
        assert!(close(all.volts, 1.12));
        let other_ccd = fx.unattributed(load(0, &[0, 1, 2, 3]), 2, 1.0).unwrap();
        assert!(close(other_ccd.volts, 1.3));
    }

    #[test]
    fn an_all_core_failure_targets_each_ccd_separately() {
        let mut fx = Fx::new(3);
        let p = fx.profile(&[-25, -30, -20, -30]);
        let lanes = [(0, 1.118), (1, 1.09), (2, 1.236), (3, 1.19)];
        for at in 1..=3 {
            fx.pass(&record(at, class(0, &[0, 1, 2, 3], 120), p, &lanes, &[]));
        }
        let all = load(0, &[0, 1, 2, 3]);
        let first = fx.unattributed(all, 0, 1.1).unwrap();
        let second = fx.unattributed(all, 2, 1.2).unwrap();
        assert_eq!(
            move_to(-30, None, OwnRequest::Measured(1.1), Some(&first), None),
            -25
        );
        assert_eq!(
            move_to(-30, None, OwnRequest::Measured(1.2), Some(&second), None),
            -20
        );
    }

    #[test]
    fn a_named_target_filters_on_the_failing_trials_clock() {
        for (name, mhz, want) in [
            ("equal clock", Some(5000), -20),
            ("lower clock", Some(4900), -29),
            ("missing pass clock", None, -29),
        ] {
            let mut fx = Fx::new(3);
            let p = fx.profile(&[-20, -30, -30, -30]);
            let clocks: Vec<(u8, i64)> = mhz.map(|m| (0, m)).into_iter().collect();
            for at in 1..=3 {
                fx.pass(&record(
                    at,
                    class(0, &[0, 1], 120),
                    p,
                    &[(0, 1.136), (1, 1.08)],
                    &clocks,
                ));
            }
            let target = fx.named(workload(0), 0, Some(5000));
            let to = move_to(-30, None, OwnRequest::Measured(1.1), target.as_ref(), None);
            assert_eq!(to, want, "{name}");
        }
    }

    #[test]
    fn a_named_target_without_any_failing_load_clock_has_no_target() {
        let mut fx = Fx::new(1);
        let p = fx.profile(&[-20, -30, -30, -30]);
        fx.pass(&record(
            1,
            class(0, &[0, 1], 120),
            p,
            &[(0, 1.136), (1, 1.08)],
            &[],
        ));
        assert_eq!(fx.named(workload(0), 0, None), None);
    }

    #[test]
    fn a_named_target_rejects_a_load_spanning_ccds() {
        let mut fx = Fx::new(3);
        let p = fx.profile(&[-30, -20, -30, -30]);
        let lanes = [(0, 1.09), (1, 1.118), (2, 1.1), (3, 1.09)];
        for at in 1..=3 {
            fx.pass(&record(
                at,
                class(0, &[0, 1, 2, 3], 120),
                p,
                &lanes,
                &[(0, 5000), (1, 5000)],
            ));
        }
        assert_eq!(fx.named(workload(0), 0, Some(5000)), None);
    }

    #[test]
    fn a_named_target_may_undercut_the_failing_request_and_still_moves_one_count() {
        let mut fx = Fx::new(1);
        let p = fx.profile(&[-20, -30, -30, -30]);
        fx.pass(&record(
            1,
            class(0, &[0, 1], 120),
            p,
            &[(0, 1.05), (1, 1.04)],
            &[(0, 5000)],
        ));
        let target = fx.named(workload(0), 0, Some(5000)).unwrap();
        assert_eq!(
            move_to(-30, None, OwnRequest::Measured(1.1), Some(&target), None),
            -29
        );
    }

    fn lane(top: f64) -> [(u8, f64); 2] {
        [(0, top), (1, 1.0)]
    }

    #[test]
    fn a_profile_needs_n_passes_and_the_lowest_voltage_among_those_wins() {
        for (n, want) in [(3, Some(1.2)), (6, None)] {
            let mut fx = Fx::new(n);
            let low = fx.profile(&[-20, -30, -30, -30]);
            let high = fx.profile(&[-10, -30, -30, -30]);
            let tops = [(1, low, 1.15), (2, low, 1.15)];
            let more = [(3, high, 1.2), (4, high, 1.2), (5, high, 1.2)];
            for (at, profile, top) in tops.into_iter().chain(more) {
                fx.pass(&record(
                    at,
                    class(0, &[0, 1], 120),
                    profile,
                    &lane(top),
                    &[],
                ));
            }
            let found = fx.unattributed(load(0, &[0, 1]), 0, 1.1);
            assert_eq!(found.as_ref().map(|t| t.volts), want, "n {n}");
            if let Some(target) = found {
                assert_eq!(target.cited, [seq(3), seq(4), seq(5)]);
            }
        }
    }

    #[test]
    fn equal_voltages_go_to_the_profile_with_the_earlier_pass() {
        let mut fx = Fx::new(2);
        let a = fx.profile(&[-20, -30, -30, -30]);
        let b = fx.profile(&[-21, -30, -30, -30]);
        for (at, profile) in [(1, b), (2, a), (3, b), (4, a)] {
            fx.pass(&record(
                at,
                class(0, &[0, 1], 120),
                profile,
                &lane(1.15),
                &[],
            ));
        }
        let target = fx.unattributed(load(0, &[0, 1]), 0, 1.0).unwrap();
        assert_eq!(target.cited, [seq(1), seq(3)]);
    }

    #[test]
    fn the_citation_keeps_the_pass_that_set_the_lowest_voltage() {
        let mut fx = Fx::new(3);
        let p = fx.profile(&[-20, -30, -30, -30]);
        for (at, top) in [(1, 1.136), (2, 1.136), (3, 1.136), (4, 1.118)] {
            fx.pass(&record(at, class(0, &[0, 1], 120), p, &lane(top), &[]));
        }
        let target = fx.unattributed(load(0, &[0, 1]), 0, 1.1).unwrap();
        assert!(close(target.volts, 1.118));
        assert_eq!(target.cited, [seq(1), seq(2), seq(4)]);
        assert_eq!(
            move_to(-30, None, OwnRequest::Measured(1.1), Some(&target), None),
            -25
        );
    }

    #[test]
    fn a_failure_hides_the_older_passes_it_rules_out_until_newer_ones_arrive() {
        let mut fx = Fx::new(1);
        let deep = fx.profile(&[-26, -30, -30, -30]);
        let shallow = fx.profile(&[-20, -30, -30, -30]);
        let here = class(0, &[0, 1], 120);
        let other = class(0, &[0, 1], 240);
        fx.pass(&record(1, here, deep, &lane(1.115), &[]));
        fx.pass(&record(2, here, shallow, &lane(1.136), &[]));
        fx.pass(&record(3, other, deep, &lane(1.125), &[]));
        let ask = |fx: &Fx| fx.unattributed(load(0, &[0, 1]), 0, 1.1).unwrap();
        assert_eq!(ask(&fx).cited, [seq(1)]);
        fx.fail(4, here, fx.profile(&[-24, -30, -30, -30]), None);
        let hidden = ask(&fx);
        assert!(close(hidden.volts, 1.125));
        assert_eq!(hidden.cited, [seq(3)]);
        fx.pass(&record(5, here, deep, &lane(1.115), &[]));
        let revived = ask(&fx);
        assert!(close(revived.volts, 1.115));
        assert_eq!(revived.cited, [seq(5)]);
    }

    #[test]
    fn an_invalidated_pass_does_not_set_the_target() {
        let mut fx = Fx::new(1);
        let low = fx.profile(&[-26, -30, -30, -30]);
        let high = fx.profile(&[-20, -30, -30, -30]);
        let here = class(0, &[0, 1], 120);
        fx.pass(&record(1, here, low, &lane(1.115), &[]));
        fx.pass(&record(2, here, high, &lane(1.136), &[]));
        let ask = |fx: &Fx| fx.unattributed(load(0, &[0, 1]), 0, 1.1);
        assert!(close(ask(&fx).unwrap().volts, 1.115));
        fx.fail(3, here, low, None);
        assert!(close(ask(&fx).unwrap().volts, 1.136));
        fx.fail(4, here, high, None);
        assert!(ask(&fx).is_none());
    }

    #[test]
    fn a_failure_the_ledger_drops_gives_its_passes_back() {
        for (named, restored) in [(Some(3), true), (None, false)] {
            let mut fx = Fx::new(2);
            let p = fx.profile(&[-26, -30, -30, -30]);
            let here = class(0, &[0, 1], 120);
            fx.pass(&record(1, here, p, &lane(1.115), &[]));
            fx.pass(&record(2, here, p, &lane(1.115), &[]));
            fx.fail(3, here, p, named);
            assert!(fx.unattributed(load(0, &[0, 1]), 0, 1.1).is_none());
            fx.reset(3);
            let target = fx.unattributed(load(0, &[0, 1]), 0, 1.1);
            assert_eq!(target.is_some(), restored, "named {named:?}");
            if let Some(target) = target {
                assert_eq!(target.cited, [seq(1), seq(2)]);
            }
        }
    }

    #[test]
    fn a_restored_pass_still_counts_with_the_passes_that_followed_the_failure() {
        let mut fx = Fx::new(3);
        let p = fx.profile(&[-26, -30, -30, -30]);
        let here = class(0, &[0, 1], 120);
        fx.pass(&record(1, here, p, &lane(1.115), &[]));
        fx.fail(2, here, p, Some(3));
        fx.pass(&record(3, here, p, &lane(1.115), &[]));
        fx.pass(&record(4, here, p, &lane(1.115), &[]));
        assert!(fx.unattributed(load(0, &[0, 1]), 0, 1.1).is_none());
        fx.reset(3);
        let target = fx.unattributed(load(0, &[0, 1]), 0, 1.1).unwrap();
        assert_eq!(target.cited, [seq(1), seq(3), seq(4)]);
    }

    #[test]
    fn a_pass_under_another_backend_does_not_count() {
        let mut fx = Fx::new(1);
        let p = fx.profile(&[-26, -30, -30, -30]);
        let other = fx
            .evidence
            .backend_id(togi_machine::Backend::Mprime, Some("elsewhere".as_ref()));
        assert_ne!(other, backend());
        assert!(fx.pass(&PassRecord {
            backend: other,
            ..record(1, class(0, &[0, 1], 120), p, &[(0, 1.2), (1, 1.1)], &[])
        }));
        let ask = |backend| {
            fx.targets
                .unattributed(fx.ask(backend), load(0, &[0, 1]), core(0), 1.0)
        };
        assert!(ask(backend()).is_none());
        assert!(ask(other).is_some());
    }

    #[test]
    fn only_r7_passes_with_request_lanes_enter_the_index() {
        let mut fx = Fx::new(1);
        let p = fx.profile(&[-26, -30, -30, -30]);
        let base = record(1, class(0, &[0, 1], 120), p, &[], &[]);
        assert!(!fx.pass(&base));
        let r6 = ClassKey::new(
            WorkloadId::new(Regime::R6, 0).unwrap(),
            set(&[0, 1]),
            Secs::new(NonZeroU32::new(120).unwrap()),
        );
        assert!(!fx.pass(&PassRecord {
            seq: seq(2),
            class: r6,
            requests: requests(&[(0, 1.1)]),
            ..base
        }));
        assert_eq!(fx.targets.groups(), 0);
    }

    #[test]
    fn a_reset_removes_the_passes_whose_load_includes_the_core() {
        let mut fx = Fx::new(1);
        let p = fx.profile(&[-26, -30, -30, -30]);
        fx.pass(&record(
            1,
            class(0, &[0, 1], 120),
            p,
            &[(0, 1.1), (1, 1.1)],
            &[],
        ));
        fx.pass(&record(
            2,
            class(0, &[2, 3], 120),
            p,
            &[(2, 1.1), (3, 1.1)],
            &[(1, 5000)],
        ));
        fx.reset(1);
        assert!(fx.named(workload(0), 0, None).is_none());
        assert!(fx.named(workload(0), 2, Some(5000)).is_some());
        assert_eq!(fx.targets.groups(), 1);
    }

    #[test]
    fn a_request_from_a_core_outside_the_topology_sets_no_ccd_top() {
        let query = |lanes: &[(u8, f64)]| {
            let mut fx = Fx::new(1);
            let p = fx.profile(&[-26, -30, -30, -30]);
            fx.pass(&record(1, class(0, &[0, 1], 120), p, lanes, &[]));
            fx.unattributed(load(0, &[0, 1]), 0, 1.1)
        };
        let alone = query(&[(0, 1.115), (1, 1.0)]);
        assert!(alone.is_some());
        assert_eq!(query(&[(0, 1.115), (1, 1.0), (9, 1.05)]), alone);
    }

    fn varied_lanes(at: u64) -> [(u8, f64); 4] {
        let wobble = f64::from(u32::try_from(at).unwrap()) * 0.000_001;
        [(0, 1.115), (1, 1.0 + wobble), (2, 1.2), (3, 1.05 + wobble)]
    }

    fn volume(count: u64) -> (usize, usize, usize, u64, Option<Target>, Option<Target>) {
        let mut fx = Fx::new(3);
        let p = fx.profile(&[-26, -30, -30, -30]);
        let all = class(0, &[0, 1, 2, 3], 120);
        for at in 1..=count {
            fx.pass(&record(
                at,
                all,
                p,
                &varied_lanes(at),
                &[(0, 5000), (1, 4000)],
            ));
        }
        assert_eq!(
            fx.targets.witnesses(),
            4 * usize::try_from(count.min(u64::from(togi_config::TrialCount::MAX))).unwrap()
        );
        fx.targets.take_visits();
        let unattributed = fx.unattributed(load(0, &[0, 1, 2, 3]), 0, 1.1);
        let named = fx.named(workload(0), 1, Some(5000));
        (
            fx.targets.entries(),
            fx.targets.segments(),
            fx.targets.groups(),
            fx.targets.take_visits(),
            unattributed,
            named,
        )
    }

    #[test]
    fn stored_size_and_query_cost_follow_distinct_evidence_not_pass_volume() {
        let small = volume(100);
        let large = volume(10_000);
        assert_eq!(small.0, 4);
        assert_eq!(small, large);
        let unattributed = large.4.unwrap();
        assert!(close(unattributed.volts, 1.115));
        assert_eq!(unattributed.cited, [seq(1), seq(2), seq(3)]);
        assert_eq!(
            volume(u64::from(togi_config::TrialCount::MAX)),
            volume(10_000)
        );
    }

    #[test]
    fn witnesses_saturate_at_the_supported_threshold_and_survive_a_larger_requirement() {
        let mut fx = Fx::new(1);
        let p = fx.profile(&[-26, -30, -30, -30]);
        let max = togi_config::TrialCount::MAX;
        for at in 1..=u64::from(max) + 100 {
            fx.pass(&record(at, class(0, &[0, 1], 120), p, &lane(1.115), &[]));
        }
        assert_eq!(fx.targets.witnesses(), 2 * usize::from(max));
        fx.n = n_of(max);
        let found = fx.unattributed(load(0, &[0, 1]), 0, 1.1).unwrap();
        assert_eq!(
            found.cited,
            (1..=u64::from(max)).map(seq).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_failure_adds_one_segment_per_entry_not_one_per_pass() {
        let mut fx = Fx::new(3);
        let p = fx.profile(&[-26, -30, -30, -30]);
        let here = class(0, &[0, 1], 120);
        for at in 1..=50 {
            fx.pass(&record(at, here, p, &lane(1.115), &[]));
        }
        let before = fx.targets.segments();
        fx.fail(51, here, p, None);
        for at in 52..=100 {
            fx.pass(&record(at, here, p, &lane(1.115), &[]));
        }
        assert_eq!(fx.targets.segments(), 2 * before);
        assert_eq!(fx.targets.entries(), before);
    }

    #[test]
    fn counts_follow_the_exact_rounding_rule() {
        let top = |volts: f64| {
            Some(Target {
                volts,
                cited: Vec::new(),
            })
        };
        let own = OwnRequest::Measured(1.1);
        assert_eq!(backoff_counts(own, None, None), 1);
        assert_eq!(backoff_counts(own, top(1.05).as_ref(), None), 1);
        assert_eq!(backoff_counts(own, top(1.1).as_ref(), None), 1);
        assert_eq!(
            backoff_counts(own, top(1.1 + VOLTS_PER_COUNT).as_ref(), None),
            1
        );
        assert_eq!(
            backoff_counts(own, top(1.1 + VOLTS_PER_COUNT + 0.0001).as_ref(), None),
            2
        );
        assert_eq!(
            backoff_counts(own, top(1.1 + 10.0 * VOLTS_PER_COUNT).as_ref(), None),
            10
        );
        assert_eq!(
            backoff_counts(OwnRequest::Measured(1.09447), top(1.1539).as_ref(), None),
            17
        );
    }

    #[test]
    fn an_offset_proxy_moves_one_count_whatever_the_target_and_rail() {
        let target = Some(Target {
            volts: 1.5,
            cited: Vec::new(),
        });
        assert_eq!(
            backoff_counts(OwnRequest::OffsetProxy, target.as_ref(), None),
            1
        );
        assert_eq!(
            backoff_counts(OwnRequest::OffsetProxy, target.as_ref(), Some(2.0)),
            1
        );
        assert_eq!(backoff_counts(OwnRequest::OffsetProxy, None, Some(2.0)), 1);
    }

    #[test]
    fn a_step_down_also_needs_the_counts_that_rise_above_the_rail() {
        let own = OwnRequest::Measured(1.15);
        assert_eq!(backoff_counts(own, None, Some(1.2)), 14);
        assert_eq!(move_to(-30, None, own, None, Some(1.2)), -16);
        assert_eq!(backoff_counts(own, None, Some(1.15)), 1);
        assert_eq!(backoff_counts(own, None, Some(1.15 + VOLTS_PER_COUNT)), 2);
        let far = Some(Target {
            volts: 1.236,
            cited: Vec::new(),
        });
        assert_eq!(backoff_counts(own, far.as_ref(), Some(1.2)), 24);
        assert_eq!(move_to(-30, None, own, far.as_ref(), Some(1.2)), -6);
    }

    #[test]
    fn a_step_down_over_a_shifted_full_measurement_aims_above_the_shifted_top() {
        let topo = two_ccds();
        let mut store = Measurements::default();
        let source = profile_of(&topo, &[-5, -30, -30, -30]);
        store.record(
            load(0, &[0, 1]),
            measurement(1, source, &[(0, 1.2), (1, 1.15)]),
        );
        let failed = profile_of(&topo, &[0, -30, -30, -30]);
        let order = RequestOrder::derive(&store, &topo, load(0, &[0, 1]), &failed);
        let ccd = order.ccd(CcdId(0)).unwrap();
        let movable = ccd.first_movable(&failed).unwrap();
        assert!(movable.stepped_down());
        assert!(close(movable.rail, 1.2 + 5.0 * VOLTS_PER_COUNT));
        let own = OwnRequest::Measured(volts(&order, 1));
        let to = move_to(-30, None, own, None, Some(movable.rail));
        assert_eq!(to, -11);
        assert_eq!(order.sources(), [seq(1)]);
    }

    #[test]
    fn the_move_is_at_least_one_count_past_the_failure_point_and_never_past_zero() {
        assert_eq!(backoff_move(off(-30), None, 1).to, off(-29));
        assert_eq!(backoff_move(off(-30), None, 1).failure_point, off(-30));
        let prior = backoff_move(off(-30), Some(off(-25)), 1);
        assert_eq!((prior.failure_point, prior.to), (off(-25), off(-24)));
        let deeper_prior = backoff_move(off(-30), Some(off(-40)), 1);
        assert_eq!(
            (deeper_prior.failure_point, deeper_prior.to),
            (off(-30), off(-29))
        );
        assert_eq!(backoff_move(off(-3), None, 17).to, off(0));
        assert_eq!(backoff_move(off(-30), None, i64::MAX).to, off(0));
        assert_eq!(backoff_move(off(0), None, 1).to, off(0));
    }
}

mod escalation {
    use super::*;

    const A: (usize, &[u8]) = (0, &[0, 1]);
    const B: (usize, &[u8]) = (1, &[0, 1]);
    const C: (usize, &[u8]) = (0, &[2, 3]);

    fn load_named(letter: char) -> LoadKey {
        let (index, ids) = match letter.to_ascii_lowercase() {
            'a' => A,
            'b' => B,
            'c' => C,
            other => panic!("no load {other}"),
        };
        load(index, ids)
    }

    #[derive(Debug, PartialEq)]
    enum Answer {
        Backoff(u32),
        Locate(Option<u32>),
        Nothing,
    }

    fn play(ops: &str) -> Answer {
        let topo = two_ccds();
        let profile = profile_of(&topo, &[-30, -30, -30, -30]);
        let mut strikes = Strikes::default();
        let mut next = 0;
        let mut last = Answer::Nothing;
        for op in ops.chars() {
            let key = load_named(op);
            if op.is_ascii_lowercase() {
                strikes.pass(key);
                continue;
            }
            next += 1;
            let failure = seq(next);
            let affected = [topo.ccd_of(key.loaded.iter().next().unwrap()).unwrap()];
            last = match strikes.escalation(&topo, key, &profile, &affected) {
                Some(Escalation::Backoffs { failures, .. }) => Answer::Locate(Some(failures)),
                Some(Escalation::LoadedAtZero) => Answer::Locate(None),
                Some(Escalation::Covered { .. }) => panic!("strikes alone never cover a load"),
                None => {
                    let ordinal = strikes.next_ordinal(key);
                    next += 1;
                    strikes.backoff(key, BackoffKind::Unattributed, seq(next), failure);
                    Answer::Backoff(ordinal)
                }
            };
        }
        last
    }

    #[test]
    fn counts_since_the_loads_latest_pass_and_escalates_the_next_failure() {
        for (name, ops, want) in [
            ("first failure of a load backs off", "A", Answer::Backoff(1)),
            ("one backoff is not enough", "AA", Answer::Backoff(2)),
            (
                "two backoffs escalate the next failure",
                "AAA",
                Answer::Locate(Some(2)),
            ),
            ("a pass resets the count", "AAaA", Answer::Backoff(1)),
            (
                "a pass resets it again after two more",
                "AAaAA",
                Answer::Backoff(2),
            ),
            (
                "two backoffs after a pass escalate",
                "AAaAAA",
                Answer::Locate(Some(2)),
            ),
            (
                "another workload counts separately",
                "AAB",
                Answer::Backoff(1),
            ),
            (
                "other loaded cores count separately",
                "AAC",
                Answer::Backoff(1),
            ),
            (
                "alternating loads keep separate counts",
                "ABABA",
                Answer::Locate(Some(2)),
            ),
            (
                "alternating loads stay below the count",
                "ABAB",
                Answer::Backoff(2),
            ),
            (
                "a pass of another load does not reset",
                "AAbA",
                Answer::Locate(Some(2)),
            ),
        ] {
            assert_eq!(play(ops), want, "{name}");
        }
        assert_eq!(ESCALATE_AFTER, 2);
    }

    #[test]
    fn one_failure_that_backed_off_a_core_per_ccd_counts_once() {
        let mut strikes = Strikes::default();
        let all = load(0, &[0, 1, 2, 3]);
        strikes.backoff(all, BackoffKind::Unattributed, seq(2), seq(1));
        strikes.backoff(all, BackoffKind::Unattributed, seq(3), seq(1));
        assert_eq!(strikes.failures(all), 1);
        strikes.backoff(all, BackoffKind::Unattributed, seq(5), seq(4));
        assert_eq!(strikes.failures(all), 2);
    }

    #[test]
    fn only_unattributed_backoffs_count() {
        let mut strikes = Strikes::default();
        let key = load(0, &[0, 1]);
        for (at, kind) in [
            BackoffKind::Named,
            BackoffKind::LocatedNamed,
            BackoffKind::Deepening,
            BackoffKind::Hold,
        ]
        .into_iter()
        .enumerate()
        {
            let at = u64::try_from(at).unwrap();
            assert!(!kind.counts());
            strikes.backoff(key, kind, seq(10 + at), seq(1 + at));
        }
        assert_eq!(strikes.failures(key), 0);
        assert!(BackoffKind::Unattributed.counts());
    }

    #[test]
    fn replaying_the_same_backoffs_rebuilds_the_same_count() {
        let build = || {
            let mut strikes = Strikes::default();
            strikes.backoff(load(0, &[0, 1]), BackoffKind::Unattributed, seq(2), seq(1));
            strikes.backoff(load(0, &[0, 1]), BackoffKind::Unattributed, seq(4), seq(3));
            strikes
        };
        assert_eq!(build(), build());
        let topo = two_ccds();
        let profile = profile_of(&topo, &[-30, -30, -30, -30]);
        let found = build().escalation(&topo, load(0, &[0, 1]), &profile, &[CcdId(0)]);
        assert_eq!(
            found,
            Some(Escalation::Backoffs {
                failures: 2,
                decisions: vec![seq(2), seq(4)]
            })
        );
    }

    #[test]
    fn escalates_at_once_when_no_loaded_core_can_step_back_while_an_unloaded_one_can() {
        let topo = two_ccds();
        let strikes = Strikes::default();
        let key = load(0, &[0, 1]);
        let stuck = profile_of(&topo, &[0, 0, -30, -30]);
        assert_eq!(
            strikes.escalation(&topo, key, &stuck, &[CcdId(0)]),
            Some(Escalation::LoadedAtZero)
        );
        let movable = profile_of(&topo, &[0, -30, -30, -30]);
        assert_eq!(strikes.escalation(&topo, key, &movable, &[CcdId(0)]), None);
        let all_zero = profile_of(&topo, &[0, 0, 0, 0]);
        assert_eq!(strikes.escalation(&topo, key, &all_zero, &[CcdId(0)]), None);
    }

    #[test]
    fn another_affected_ccd_with_a_movable_loaded_core_is_not_stuck() {
        let topo = two_ccds();
        let all = load(0, &[0, 1, 2, 3]);
        let profile = profile_of(&topo, &[0, 0, -30, -30]);
        assert!(loaded_stuck_at_zero(&topo, all, &profile, &[CcdId(0)]));
        assert!(!loaded_stuck_at_zero(
            &topo,
            all,
            &profile,
            &[CcdId(0), CcdId(1)]
        ));
        assert!(unloaded_off_zero(&topo, load(0, &[0, 1]), &profile));
        assert!(!unloaded_off_zero(&topo, all, &profile));
    }

    #[test]
    fn the_reasons_carry_what_the_journal_must_say() {
        let backoffs = Escalation::Backoffs {
            failures: 2,
            decisions: vec![seq(12), seq(30)],
        }
        .to_string();
        for token in [
            "backed off 2 times since its last passing trial",
            "#12, #30",
            "still fails",
        ] {
            assert!(backoffs.contains(token), "{backoffs:?} lacks {token:?}");
        }
        let stuck = Escalation::LoadedAtZero.to_string();
        for token in ["every loaded core", "CO 0", "cannot help"] {
            assert!(stuck.contains(token), "{stuck:?} lacks {token:?}");
        }
    }

    #[test]
    fn a_reset_of_a_loaded_core_restarts_the_count_and_one_outside_the_load_keeps_it() {
        let build = || {
            let mut strikes = Strikes::default();
            strikes.backoff(load(0, &[0, 1]), BackoffKind::Unattributed, seq(2), seq(1));
            strikes.backoff(load(0, &[0, 1]), BackoffKind::Unattributed, seq(4), seq(3));
            strikes
        };
        let mut loaded = build();
        loaded.reset_core(core(0), |_| None);
        assert_eq!(loaded.failures(load(0, &[0, 1])), 0);
        let mut outside = build();
        outside.reset_core(core(3), |_| None);
        assert_eq!(outside.failures(load(0, &[0, 1])), 2);
    }

    #[test]
    fn a_reset_drops_the_strikes_whose_failure_named_the_core() {
        let mut strikes = Strikes::default();
        let key = load(0, &[0, 1]);
        strikes.backoff(key, BackoffKind::Unattributed, seq(2), seq(1));
        strikes.backoff(key, BackoffKind::Unattributed, seq(4), seq(3));
        strikes.reset_core(core(3), |failure| (failure == seq(3)).then(|| core(3)));
        assert_eq!(strikes.failures(key), 1);
    }
}

mod status {
    use super::*;

    fn one_ccd(ids: &[u8]) -> Topology {
        crate::tests::topology(ids)
    }

    #[test]
    fn counts_passes_by_workload_offset_and_top_requester() {
        let topo = one_ccd(&[3, 7]);
        let mut status = SelfSufficiency::default();
        let loaded = set(&[3, 7]);
        status.pass(
            backend(),
            workload(0),
            loaded,
            set(&[3]),
            &profile_of(&topo, &[-21, -30]),
        );
        status.pass(
            backend(),
            workload(0),
            loaded,
            set(&[7]),
            &profile_of(&topo, &[-19, -29]),
        );
        let at = |w, c: u8, current| status.passes(backend(), workload(w), core(c), off(current));
        assert_eq!(at(0, 3, -20), 1);
        assert_eq!(at(0, 7, -30), 0);
        assert_eq!(at(1, 3, -20), 0);
        assert_eq!(at(0, 3, -22), 0);
        assert!(status.is_self_sufficient(backend(), workload(0), core(3), off(-20)));
        assert!(!status.is_self_sufficient(backend(), workload(0), core(7), off(-30)));
    }

    #[test]
    fn counts_equal_and_deeper_offsets_and_not_shallower_ones() {
        let topo = one_ccd(&[0]);
        let mut status = SelfSufficiency::default();
        let loaded = set(&[0]);
        for offset in [-10, -20, -20, -35, -50, 0] {
            status.pass(
                backend(),
                workload(0),
                loaded,
                loaded,
                &profile_of(&topo, &[offset]),
            );
        }
        let at = |current| status.passes(backend(), workload(0), core(0), off(current));
        assert_eq!(at(0), 6);
        assert_eq!(at(-10), 5);
        assert_eq!(at(-20), 4);
        assert_eq!(at(-21), 2);
        assert_eq!(at(-35), 2);
        assert_eq!(at(-36), 1);
        assert_eq!(at(-50), 1);
    }

    #[test]
    fn pools_across_loaded_sets_and_ignores_other_backends() {
        let topo = one_ccd(&[0, 1]);
        let mut status = SelfSufficiency::default();
        let p = profile_of(&topo, &[-30, -30]);
        status.pass(backend(), workload(0), set(&[0, 1]), set(&[0]), &p);
        status.pass(backend(), workload(0), set(&[0]), set(&[0]), &p);
        let mut evidence = Evidence::new(topo.all());
        let other = evidence.backend_id(togi_machine::Backend::Mprime, Some("elsewhere".as_ref()));
        status.pass(other, workload(0), set(&[0]), set(&[0]), &p);
        assert_eq!(status.passes(backend(), workload(0), core(0), off(-30)), 2);
        assert_eq!(status.passes(other, workload(0), core(0), off(-30)), 1);
    }

    #[test]
    fn a_reset_removes_every_pass_whose_load_includes_the_core() {
        let topo = one_ccd(&[3, 7]);
        let mut status = SelfSufficiency::default();
        let p = profile_of(&topo, &[-21, -30]);
        status.pass(backend(), workload(0), set(&[3, 7]), set(&[3]), &p);
        status.pass(backend(), workload(0), set(&[7]), set(&[7]), &p);
        status.reset_core(core(3));
        assert_eq!(status.passes(backend(), workload(0), core(3), off(-20)), 0);
        assert_eq!(status.passes(backend(), workload(0), core(7), off(-20)), 1);
        status.reset_core(core(7));
        assert_eq!(status, SelfSufficiency::default());
    }

    #[test]
    fn a_reset_of_another_loaded_core_removes_the_top_requesters_pass_too() {
        let topo = one_ccd(&[3, 7]);
        let mut status = SelfSufficiency::default();
        let p = profile_of(&topo, &[-21, -30]);
        status.pass(backend(), workload(0), set(&[3, 7]), set(&[3]), &p);
        status.reset_core(core(7));
        assert_eq!(status.passes(backend(), workload(0), core(3), off(-20)), 0);
    }
}

mod props {
    use super::*;

    const LOADS: [&[u8]; 6] = [&[0], &[1], &[0, 1], &[2, 3], &[1, 2], &[0, 1, 2, 3]];

    #[derive(Clone, Debug)]
    enum Op {
        Record {
            workload: usize,
            loaded: usize,
            profile: [i64; 4],
            volts: [f64; 4],
        },
        Reset(u8),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            9 => (0..2_usize, 0..LOADS.len(), uniform4(-50..=0_i64), uniform4(1.0..1.3_f64)).prop_map(
                |(workload, loaded, profile, volts)| Op::Record { workload, loaded, profile, volts }
            ),
            1 => (0..4_u8).prop_map(Op::Reset),
        ]
    }

    struct Naive {
        all: Vec<(LoadKey, Measurement)>,
    }

    impl Naive {
        fn newest(&self, key: LoadKey) -> Option<&Measurement> {
            self.all
                .iter()
                .filter(|(load, _)| *load == key)
                .map(|(_, m)| m)
                .max_by_key(|m| m.seq())
        }
    }

    fn naive_order(
        naive: &Naive,
        topo: &Topology,
        key: LoadKey,
        profile: &Profile,
    ) -> Vec<(CoreId, Option<f64>, Option<Seq>)> {
        let exact = naive.newest(key);
        let mut out = Vec::new();
        for &(_, part) in topo.ccds() {
            let source = exact.or_else(|| naive.newest(LoadKey::new(key.workload, part)));
            for id in part.iter().filter(|&id| key.loaded.contains(id)) {
                let lane = source.and_then(|m| {
                    m.request(id).map(|request| {
                        let shift = profile.at(id).signed() - m.profile().at(id).signed();
                        (request + VOLTS_PER_COUNT * f64::from(shift), m.seq())
                    })
                });
                out.push((id, lane.map(|l| l.0), lane.map(|l| l.1)));
            }
        }
        out
    }

    proptest! {
        #![proptest_config(crate::prop_tests::seeded(128))]

        #[test]
        fn the_newest_only_store_answers_like_a_list_of_every_measurement(
            ops in proptest::collection::vec(op(), 1..40),
            target in uniform4(-50..=0_i64),
        ) {
            let topo = two_ccds();
            let mut store = Measurements::default();
            let mut naive = Naive { all: Vec::new() };
            for (at, op) in ops.iter().enumerate() {
                match op {
                    Op::Record { workload, loaded, profile, volts } => {
                        let key = load(*workload, LOADS[*loaded]);
                        let lanes: Vec<(u8, f64)> =
                            LOADS[*loaded].iter().map(|&id| (id, volts[usize::from(id)])).collect();
                        let m = measurement(
                            u64::try_from(at).unwrap() + 1,
                            profile_of(&topo, profile),
                            &lanes,
                        );
                        store.record(key, m.clone());
                        naive.all.push((key, m));
                    }
                    Op::Reset(id) => {
                        store.reset_core(core(*id));
                        naive.all.retain(|(key, _)| !key.loaded.contains(core(*id)));
                    }
                }
            }
            let target = profile_of(&topo, &target);
            for workload_index in 0..2 {
                for ids in LOADS {
                    let key = load(workload_index, ids);
                    prop_assert_eq!(store.newest(key), naive.newest(key));
                    let order = RequestOrder::derive(&store, &topo, key, &target);
                    for (id, volts, source) in naive_order(&naive, &topo, key, &target) {
                        if let Some(expected) = volts {
                            prop_assert!(close(order.volts(id).unwrap(), expected));
                            prop_assert_eq!(order.lane(id), Some(Lane::Measured { source: source.unwrap() }));
                        } else {
                            prop_assert_eq!(order.volts(id), None);
                            prop_assert_eq!(order.lane(id), Some(Lane::OffsetProxy));
                        }
                    }
                }
            }
        }
    }

    const SETS: [&[u8]; 2] = [&[0, 1], &[0, 1, 2, 3]];
    const LEVELS: [i64; 3] = [-30, -20, -10];
    const TOPS: [f64; 4] = [1.1, 1.115, 1.12, 1.15];
    const FAILING_TOPS: [f64; 4] = [1.0, 1.1, 1.115, 1.13];
    const DURATIONS: [u32; 2] = [120, 240];

    #[derive(Clone, Debug)]
    enum Event {
        Pass {
            duration: usize,
            set: usize,
            levels: [usize; 4],
            tops: [usize; 4],
            mhz: [i64; 2],
        },
        Fail {
            duration: usize,
            set: usize,
            levels: [usize; 4],
            named: Option<u8>,
        },
        Reset(u8),
    }

    fn event() -> impl Strategy<Value = Event> {
        let mhz = prop_oneof![Just(4900_i64), Just(5000)];
        prop_oneof![
            6 => (
                0..DURATIONS.len(),
                0..SETS.len(),
                uniform4(0..LEVELS.len()),
                uniform4(0..TOPS.len()),
                [mhz.clone(), mhz],
            )
                .prop_map(|(duration, set, levels, tops, mhz)| Event::Pass {
                    duration,
                    set,
                    levels,
                    tops,
                    mhz,
                }),
            2 => (
                0..DURATIONS.len(),
                0..SETS.len(),
                uniform4(0..LEVELS.len()),
                proptest::option::of(0..4_u8),
            )
                .prop_map(|(duration, set, levels, named)| Event::Fail {
                    duration,
                    set,
                    levels,
                    named,
                }),
            1 => (0..4_u8).prop_map(Event::Reset),
        ]
    }

    fn scan(
        fx: &Fx,
        passes: &[PassRecord],
        pick: impl Fn(&PassRecord) -> Option<f64>,
    ) -> Option<(f64, Seq, Vec<Seq>)> {
        type PassGroup = ((LoadKey, Profile), Vec<(Seq, f64)>);
        let mut groups: Vec<PassGroup> = Vec::new();
        for pass in passes {
            let valid = fx
                .evidence
                .newest_failure(pass.class, &pass.profile, None)
                .unwrap()
                .is_none_or(|failure| failure < pass.seq);
            let Some(top) = pick(pass).filter(|_| valid) else {
                continue;
            };
            match groups
                .iter_mut()
                .find(|(shape, _)| *shape == (pass.class.load(), pass.profile))
            {
                Some((_, found)) => found.push((pass.seq, top)),
                None => groups.push(((pass.class.load(), pass.profile), vec![(pass.seq, top)])),
            }
        }
        let n = usize::from(fx.n.get());
        groups
            .into_iter()
            .filter(|(_, found)| found.len() >= n)
            .map(|(_, found)| {
                let (lowest_seq, volts) = found
                    .iter()
                    .copied()
                    .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
                    .unwrap();
                let mut seqs: Vec<Seq> = found.iter().map(|&(seq, _)| seq).collect();
                seqs.sort_unstable();
                seqs.truncate(n);
                let earliest = seqs[0];
                if !seqs.contains(&lowest_seq) {
                    seqs[n - 1] = lowest_seq;
                    seqs.sort_unstable();
                }
                (volts, earliest, seqs)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
    }

    proptest! {
        #![proptest_config(crate::prop_tests::seeded(256))]

        #[test]
        fn the_index_answers_like_a_scan_of_every_pass(
            n in 1..=3_u16,
            events in proptest::collection::vec(event(), 1..60),
        ) {
            let mut fx = Fx::new(n);
            let mut passes: Vec<PassRecord> = Vec::new();
            for (at, event) in events.iter().enumerate() {
                let at = u64::try_from(at).unwrap() + 1;
                match event {
                    Event::Pass { duration, set, levels, tops, mhz } => {
                        let key = class(0, SETS[*set], DURATIONS[*duration]);
                        let lanes: Vec<(u8, f64)> = SETS[*set]
                            .iter()
                            .map(|&id| (id, TOPS[tops[usize::from(id)]]))
                            .collect();
                        let levels = levels.map(|level| LEVELS[level]);
                        let pass = record(
                            at,
                            key,
                            fx.profile(&levels),
                            &lanes,
                            &[(0, mhz[0]), (1, mhz[1])],
                        );
                        fx.pass(&pass);
                        passes.push(pass);
                    }
                    Event::Fail { duration, set, levels, named } => {
                        let key = class(0, SETS[*set], DURATIONS[*duration]);
                        let levels = levels.map(|level| LEVELS[level]);
                        fx.fail(at, key, fx.profile(&levels), *named);
                    }
                    Event::Reset(id) => {
                        fx.reset(*id);
                        passes.retain(|pass| !pass.class.loaded().contains(core(*id)));
                    }
                }
            }
            for set in SETS {
                let wanted = load(0, set);
                for &moved in set {
                    let ccd = fx.topo.ccd_of(core(moved));
                    for failing in FAILING_TOPS {
                        let expected = scan(&fx, &passes, |pass| {
                            if pass.class.load() != wanted {
                                return None;
                            }
                            let on_ccd: PerCore<f64> = pass
                                .requests
                                .iter()
                                .filter(|&(id, _)| fx.topo.ccd_of(id) == ccd)
                                .collect();
                            top(&on_ccd).filter(|&highest| highest > failing)
                        });
                        let found = fx.unattributed(wanted, moved, failing);
                        prop_assert_eq!(
                            found.map(|t| (t.volts.to_bits(), t.cited)),
                            expected.map(|(volts, _, cited)| (volts.to_bits(), cited)),
                        );
                    }
                }
            }
            for moved in 0..4_u8 {
                let ccd = fx.topo.ccd_of(core(moved)).unwrap();
                for failing in [None, Some(4900), Some(5000)] {
                    let expected = scan(&fx, &passes, |pass| {
                        if !pass.class.loaded().contains(core(moved)) || !pass.class.loaded().iter()
                            .all(|id| fx.topo.ccd_of(id) == Some(ccd)) {
                            return None;
                        }
                        let clock = pass.ccd_mhz.get(&ccd).copied();
                        let admitted = failing.zip(clock).is_some_and(|(floor, clock)| clock >= floor - 25);
                        let on_ccd: PerCore<f64> = pass.requests.iter()
                            .filter(|&(id, _)| fx.topo.ccd_of(id) == Some(ccd)).collect();
                        top(&on_ccd).filter(|_| admitted)
                    });
                    let found = fx.named(workload(0), moved, failing);
                    prop_assert_eq!(
                        found.map(|t| (t.volts.to_bits(), t.cited)),
                        expected.map(|(volts, _, cited)| (volts.to_bits(), cited)),
                    );
                }
            }
        }
    }

    #[test]
    fn the_target_offset_type_bounds_every_move() {
        for applied in [-50, -30, 0] {
            for counts in [1, 5, 60, i64::MAX] {
                let to = backoff_move(TargetOffset::new(applied).unwrap(), None, counts).to;
                assert!((-50..=0).contains(&to.get()));
                assert!(!to.deeper_than(TargetOffset::new(applied).unwrap()));
            }
        }
    }
}

mod counts_above_tests {
    use super::*;

    #[test]
    fn lifts_a_request_strictly_over_the_top() {
        for (v, top, want) in [
            (1.100, 1.100, 1),
            (1.100, 1.050, 1),
            (1.100, 1.100 + VOLTS_PER_COUNT - 0.0001, 1),
            (1.100, 1.100 + VOLTS_PER_COUNT, 2),
            (1.100, 1.100 + 2.0 * VOLTS_PER_COUNT + 0.0001, 3),
            (1.15, 1.2, 14),
        ] {
            assert_eq!(counts_above(v, top), want, "counts_above({v}, {top})");
        }
    }

    #[test]
    fn a_finite_request_gap_beyond_the_count_range_clamps_to_the_largest_count() {
        assert_eq!(counts_above(1.0, 1e20), i64::MAX);
        assert_eq!(counts_above(1.0, f64::MAX / 2.0), i64::MAX);
        assert_eq!(
            backoff_counts(OwnRequest::Measured(1.0), None, Some(1e20)),
            i64::MAX
        );
        let applied = TargetOffset::new(-30).unwrap();
        assert_eq!(
            backoff_move(
                applied,
                None,
                backoff_counts(OwnRequest::Measured(1.0), None, Some(1e20))
            )
            .to,
            TargetOffset::new(0).unwrap()
        );
    }
}

#[test]
fn r7_reasons_render_each_voltage_basis_and_locate_provenance() {
    use crate::r7::reason::{Basis, R7Reason};
    use crate::r7::respond::LocateReason;

    for (basis, text) in [
        (Basis::NoTelemetry { offset: -30 }, "no request telemetry"),
        (
            Basis::Uncovered {
                offset: -30,
                sources: vec![seq(3)],
            },
            "request measurements [3] do not cover it",
        ),
        (Basis::NoPass { request: 1.1 }, "no qualifying pass"),
        (
            Basis::Met {
                request: 1.2,
                target: 1.1,
            },
            "already met the passed target 1.100 V",
        ),
        (
            Basis::Toward {
                request: 1.1,
                target: 1.2,
            },
            "request 1.100 V to 1.200 V",
        ),
    ] {
        let reason = R7Reason {
            failure: seq(10),
            core: core(0),
            basis,
            counts: 1,
            notes: Vec::new(),
            rail: None,
        }
        .to_string();
        assert!(reason.contains("after failure #10: core 00"));
        assert!(reason.contains(text), "{reason}");
        assert!(reason.contains("1 count"));
    }
    for rerun_passed in [None, Some(seq(19))] {
        let reason = LocateReason {
            rerun_passed,
            escalation: Some(Escalation::Backoffs {
                failures: 2,
                decisions: vec![seq(12), seq(30)],
            }),
        };
        let mut expected = rerun_passed.into_iter().collect::<Vec<_>>();
        expected.extend([seq(12), seq(30)]);
        assert_eq!(reason.cause(), expected);
        let text = reason.to_string();
        assert!(text.contains("located on the unloaded cores"));
        assert!(text.contains("#12, #30"));
        assert_eq!(text.contains("CO 0 passed (#19)"), rerun_passed.is_some());
    }
    for rerun_passed in [None, Some(seq(19))] {
        let reason = LocateReason {
            rerun_passed,
            escalation: Some(Escalation::Covered {
                passes: vec![seq(12), seq(30)],
            }),
        };
        let mut expected = rerun_passed.into_iter().collect::<Vec<_>>();
        expected.extend([seq(12), seq(30)]);
        assert_eq!(reason.cause(), expected);
        let text = reason.to_string();
        assert!(text.contains("located on the unloaded cores"), "{text}");
        assert!(
            text.ends_with(
                "; the load already passed with every loaded core as deep or deeper and an \
                 unloaded core shallower (#12, #30), so its loaded cores pass on their own and \
                 stepping one back cannot be what it needs"
            ),
            "{text}"
        );
        assert_eq!(text.contains("CO 0 passed (#19)"), rerun_passed.is_some());
    }
}
