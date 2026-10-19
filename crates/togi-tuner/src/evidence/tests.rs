use std::num::{NonZeroU16, NonZeroU32};
use std::path::Path;

use test_case::test_case;
use proptest::prelude::*;
use togi_journal::Seq;
use togi_machine::{Backend, Condition, CoreId, CoreSet, REGIMES, Regime, WorkloadId, workloads};

use crate::tests::{core, profile, topology};
use crate::{
    BackendId, ClassKey, Evidence, IdleContradiction, Observation, Profile, Secs, SessionRef,
    Topology, TopologyMismatch, Window,
};

fn seq(n: u64) -> Seq {
    Seq::new(n).unwrap()
}

fn needed(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap()
}

fn set(ids: &[u8]) -> CoreSet {
    ids.iter().map(|&id| core(id)).collect()
}

fn class(regime: Regime, index: usize, loaded: &[u8], duration: u32) -> ClassKey {
    ClassKey::new(
        WorkloadId::new(regime, index).unwrap(),
        set(loaded),
        Secs::new(NonZeroU32::new(duration).unwrap()),
    )
}

const ANY: Window = Window::LiveOrCarried { since: None };

struct Fx {
    ev: Evidence,
    topo: Topology,
    next: u64,
}

impl Fx {
    fn new(ids: &[u8]) -> Self {
        let topo = topology(ids);
        Self {
            ev: Evidence::new(topo.all()),
            topo,
            next: 0,
        }
    }

    fn p(&self, values: &[i64]) -> Profile {
        profile(&self.topo, values)
    }

    fn tick(&mut self) -> Seq {
        self.next += 1;
        seq(self.next)
    }

    fn obs(seq: Seq, class: ClassKey, profile: &Profile, condition: Condition) -> Observation<'_> {
        Observation {
            seq,
            class,
            profile,
            condition,
            named: None,
        }
    }

    fn pass_as(&mut self, class: ClassKey, p: &Profile, condition: Condition) -> Seq {
        let at = self.tick();
        let backend = self.ev.active_backend(class.workload().backend());
        self.ev
            .record_pass(Self::obs(at, class, p, condition), backend)
            .unwrap();
        at
    }

    fn pass(&mut self, class: ClassKey, p: &Profile) -> Seq {
        self.pass_as(class, p, Condition::Together)
    }

    fn passes(&mut self, class: ClassKey, p: &Profile, times: usize) -> Vec<Seq> {
        (0..times).map(|_| self.pass(class, p)).collect()
    }

    fn fail(&mut self, class: ClassKey, p: &Profile) -> Seq {
        let at = self.tick();
        self.ev
            .record_failure(Self::obs(at, class, p, Condition::Together))
            .unwrap();
        at
    }

    fn idle(&mut self, p: &Profile) -> Seq {
        let at = self.tick();
        self.ev.record_idle_failure(at, p).unwrap();
        at
    }

    fn carried_pass(&mut self, class: ClassKey, p: &Profile, session: &str) -> Seq {
        let at = self.tick();
        let backend = self.ev.active_backend(class.workload().backend());
        self.ev
            .record_carried_pass(
                Self::obs(at, class, p, Condition::Together),
                backend,
                SessionRef::new(session),
            )
            .unwrap();
        at
    }

    fn carried_fail(&mut self, class: ClassKey, p: &Profile, session: &str) -> Seq {
        let at = self.tick();
        self.ev
            .record_carried_failure(
                Self::obs(at, class, p, Condition::Together),
                SessionRef::new(session),
            )
            .unwrap();
        at
    }

    fn carried_idle(&mut self, p: &Profile, session: &str) -> Seq {
        let at = self.tick();
        self.ev
            .record_carried_idle_failure(at, p, SessionRef::new(session))
            .unwrap();
        at
    }

    fn valid(&self, class: ClassKey, p: &Profile) -> u32 {
        self.ev.count(class, p, ANY, needed(1)).unwrap().valid
    }
}

#[test]
fn evidence_validity() {
    let mut fx = Fx::new(&[0, 1]);
    let k = class(Regime::R6, 0, &[0, 1], 120);
    let rows: [(&[i64], bool); 5] = [
        (&[-20, -10], true),
        (&[-19, -10], true),
        (&[-20, -10], false),
        (&[-22, -10], true),
        (&[-20, -12], true),
    ];
    let mut seqs = Vec::new();
    for (values, passed) in rows {
        let p = fx.p(values);
        seqs.push(if passed {
            fx.pass(k, &p)
        } else {
            fx.fail(k, &p)
        });
    }
    assert_eq!(fx.valid(k, &fx.p(&[-19, -10])), 4);
    assert_eq!(fx.valid(k, &fx.p(&[-21, -10])), 1);
    assert_eq!(fx.valid(k, &fx.p(&[-20, -11])), 1);
    assert_eq!(
        fx.ev
            .failing_seq(k, &fx.p(&[-20, -10]), Some(seqs[1]), needed(3))
            .unwrap(),
        Some(seqs[2])
    );
    let idle = fx.idle(&fx.p(&[-20, -10]));
    let other_duration = class(Regime::R6, 0, &[0, 1], 900);
    assert_eq!(
        fx.ev
            .failing_seq(other_duration, &fx.p(&[-20, -10]), None, needed(3))
            .unwrap(),
        Some(idle)
    );
    let parked = fx.idle(&fx.p(&[-20, -10]));
    assert_eq!(
        fx.ev
            .failing_seq(other_duration, &fx.p(&[-20, -10]), Some(idle), needed(3))
            .unwrap(),
        Some(parked)
    );
}

#[test]
fn idle_failure_leaves_partial_r6_and_other_regimes_alone() {
    let mut fx = Fx::new(&[0, 1]);
    let p = fx.p(&[-20, -10]);
    fx.idle(&p);
    for k in [
        class(Regime::R6, 0, &[0], 120),
        class(Regime::R1, 0, &[0, 1], 120),
        class(Regime::R7, 0, &[0, 1], 120),
    ] {
        assert_eq!(fx.ev.failing_seq(k, &p, None, needed(1)).unwrap(), None);
    }
}

#[test]
fn failed_before_admits_idle_failures_for_r6_only() {
    let short = Secs::new(NonZeroU32::new(120).unwrap());
    let mut fx = Fx::new(&[0, 1]);
    let deep = fx.p(&[-40, -40]);
    let live = fx.idle(&deep);
    fx.carried_idle(&deep, "old");
    let after = fx.tick();
    let before = fx.tick();
    for (regime, want) in [(Regime::R6, true), (Regime::R7, false), (Regime::R1, false)] {
        assert_eq!(
            fx.ev.failed_before(regime, short, None, before),
            want,
            "{regime:?}"
        );
    }
    assert!(!fx.ev.failed_before(Regime::R6, short, None, live));
    assert!(fx.ev.failed_before(Regime::R6, short, Some(live), before));
    assert!(fx.ev.failed_before(Regime::R6, short, Some(after), before));
}

#[test]
fn failed_before_idle_live_is_cut_by_the_reset_boundary_and_carried_is_not() {
    let short = Secs::new(NonZeroU32::new(120).unwrap());
    let deep = |fx: &Fx| fx.p(&[-40, -40]);
    let mut live = Fx::new(&[0, 1]);
    let p = deep(&live);
    live.idle(&p);
    let boundary = live.tick();
    let before = live.tick();
    assert!(live.ev.failed_before(Regime::R6, short, None, before));
    assert!(
        !live
            .ev
            .failed_before(Regime::R6, short, Some(boundary), before)
    );
    let mut carried = Fx::new(&[0, 1]);
    let p = deep(&carried);
    carried.carried_idle(&p, "old");
    let boundary = carried.tick();
    let before = carried.tick();
    assert!(
        carried
            .ev
            .failed_before(Regime::R6, short, Some(boundary), before)
    );
}

#[test]
fn monotonicity_warning() {
    let mut fx = Fx::new(&[0]);
    let k = class(Regime::R1, 0, &[0], 120);
    let p = fx.p(&[-10]);
    assert_eq!(fx.ev.monotonicity(k, &p, needed(3)).unwrap(), None);
    let expected = fx.passes(k, &p, 3);
    assert_eq!(
        fx.ev.monotonicity(k, &p, needed(3)).unwrap(),
        Some(expected.clone())
    );
    assert_eq!(fx.ev.monotonicity(k, &p, needed(4)).unwrap(), None);
    let extra = fx.pass(k, &p);
    assert_eq!(
        fx.ev.monotonicity(k, &p, needed(3)).unwrap(),
        Some(expected)
    );
    assert_eq!(
        fx.ev.count(k, &p, ANY, needed(3)).unwrap().valid,
        4,
        "witnesses are the first n of {extra:?} and its elders"
    );
}

#[test]
fn multi_core_r7_failures_contradict_nothing() {
    let mut fx = Fx::new(&[0, 1]);
    let k = class(Regime::R7, 0, &[0, 1], 120);
    let p = fx.p(&[-10, -10]);
    fx.passes(k, &p, 3);
    assert_eq!(fx.ev.monotonicity(k, &p, needed(3)).unwrap(), None);
    let single = class(Regime::R7, 0, &[0], 120);
    fx.passes(single, &p, 3);
    assert!(fx.ev.monotonicity(single, &p, needed(3)).unwrap().is_some());
}

struct IdleRow {
    name: &'static str,
    pass_profile: [i64; 2],
    durations: &'static [u32],
    cores: &'static [u8],
    warn: bool,
}

#[test]
fn idle_monotonicity_warning() {
    let rows = [
        IdleRow {
            name: "equal profile",
            pass_profile: [-10, -12],
            durations: &[120],
            cores: &[0, 1],
            warn: true,
        },
        IdleRow {
            name: "deeper passing profile",
            pass_profile: [-11, -13],
            durations: &[900],
            cores: &[0, 1],
            warn: true,
        },
        IdleRow {
            name: "shallower passing profile",
            pass_profile: [-9, -11],
            durations: &[120],
            cores: &[0, 1],
            warn: false,
        },
        IdleRow {
            name: "durations cannot pool",
            pass_profile: [-10, -12],
            durations: &[120, 900],
            cores: &[0, 1],
            warn: false,
        },
        IdleRow {
            name: "partial load cannot establish the class",
            pass_profile: [-10, -12],
            durations: &[120],
            cores: &[0],
            warn: false,
        },
    ];
    for row in rows {
        let mut fx = Fx::new(&[0, 1]);
        let pass_profile = fx.p(&row.pass_profile);
        let mut seqs = Vec::new();
        for i in 0..3 {
            let k = class(
                Regime::R6,
                0,
                row.cores,
                row.durations[i % row.durations.len()],
            );
            seqs.push(fx.pass(k, &pass_profile));
        }
        let found = fx
            .ev
            .idle_monotonicity(&fx.p(&[-10, -12]), needed(3))
            .unwrap();
        assert_eq!(found.is_some(), row.warn, "{}", row.name);
        if let Some(found) = found {
            assert_eq!(found.passes, seqs, "{}", row.name);
        }
    }
}

#[test]
fn idle_monotonicity_chooses_earliest_passing_class() {
    let mut fx = Fx::new(&[0, 1]);
    let p = fx.p(&[-10, -12]);
    let long = class(Regime::R6, 0, &[0, 1], 900);
    let short = class(Regime::R6, 0, &[0, 1], 120);
    let first = fx.passes(long, &p, 4);
    fx.passes(short, &p, 4);
    let expected = IdleContradiction {
        class: long,
        passes: first[..3].to_vec(),
    };
    for _ in 0..20 {
        assert_eq!(
            fx.ev.idle_monotonicity(&p, needed(3)).unwrap(),
            Some(expected.clone())
        );
    }
}

const MPRIME_OLD: &str = "/nix/store/old-mprime";
const MPRIME_NEW: &str = "/nix/store/new-mprime";
const YCRUNCHER_OLD: &str = "/nix/store/old-ycruncher";
const YCRUNCHER_NEW: &str = "/nix/store/new-ycruncher";

fn use_backends(ev: &mut Evidence, mprime: &str, ycruncher: &str) {
    ev.set_backend(Backend::Mprime, Some(Path::new(mprime)));
    ev.set_backend(Backend::Ycruncher, Some(Path::new(ycruncher)));
}

#[test_case(&[(MPRIME_OLD, YCRUNCHER_OLD)], 10, 10, false; "same backends")]
#[test_case(&[(MPRIME_NEW, YCRUNCHER_OLD)], 0, 10, true; "mprime updated")]
#[test_case(&[(MPRIME_OLD, YCRUNCHER_NEW)], 10, 0, false; "ycruncher updated")]
#[test_case(&[(MPRIME_NEW, YCRUNCHER_NEW)], 0, 0, true; "both updated")]
#[test_case(
    &[(MPRIME_NEW, YCRUNCHER_OLD), (MPRIME_OLD, YCRUNCHER_OLD)],
    10, 10, false; "mprime rolled back"
)]
fn evidence_is_keyed_by_backend_store_path(
    reload: &[(&str, &str)],
    mprime_valid: u32,
    ycruncher_valid: u32,
    mprime_failure_valid: bool,
) {
    let r1 = workloads(Regime::R1);
    assert_eq!(
        (r1[0].backend, r1[1].backend),
        (Backend::Mprime, Backend::Ycruncher)
    );
    let mprime = class(Regime::R1, 0, &[0], 90);
    let ycruncher = class(Regime::R1, 1, &[0], 90);
    let mut fx = Fx::new(&[0, 1]);
    use_backends(&mut fx.ev, MPRIME_OLD, YCRUNCHER_OLD);
    let p = fx.p(&[-10, -10]);
    fx.fail(mprime, &p);
    for k in [mprime, ycruncher] {
        fx.passes(k, &p, 5);
        for _ in 0..5 {
            fx.carried_pass(k, &p, "old");
        }
    }
    assert_eq!(
        fx.ev.failing_seq(mprime, &p, None, needed(5)).unwrap(),
        None,
        "passes under the recording backend outweigh the earlier failure"
    );
    for &(m, y) in reload {
        use_backends(&mut fx.ev, m, y);
    }
    assert_eq!(fx.valid(mprime, &p), mprime_valid);
    assert_eq!(fx.valid(ycruncher, &p), ycruncher_valid);
    assert_eq!(
        fx.ev
            .failing_seq(mprime, &p, None, needed(5))
            .unwrap()
            .is_some(),
        mprime_failure_valid
    );
}

#[test]
fn restoring_a_path_restores_its_backend_id() {
    let mut ev = Evidence::new(set(&[0]));
    let old = ev.backend_id(Backend::Mprime, Some(Path::new(MPRIME_OLD)));
    let new = ev.backend_id(Backend::Mprime, Some(Path::new(MPRIME_NEW)));
    let none = ev.backend_id(Backend::Mprime, None);
    assert_ne!(old, new);
    assert_ne!(old, none);
    assert_eq!(
        ev.backend_id(Backend::Mprime, Some(Path::new(MPRIME_OLD))),
        old
    );
    assert_ne!(
        ev.backend_id(Backend::Ycruncher, Some(Path::new(MPRIME_OLD))),
        old
    );
    ev.set_backend(Backend::Mprime, Some(Path::new(MPRIME_NEW)));
    assert_eq!(ev.active_backend(Backend::Mprime), new);
    ev.set_backend(Backend::Mprime, Some(Path::new(MPRIME_OLD)));
    assert_eq!(ev.active_backend(Backend::Mprime), old);
    let _: BackendId = none;
}

#[test]
fn carried_failure_invalidation() {
    for idle in [false, true] {
        let mut fx = Fx::new(&[0]);
        let regime = if idle { Regime::R6 } else { Regime::R1 };
        let k = class(regime, 0, &[0], 60);
        let p = fx.p(&[-20]);
        let shallow = fx.p(&[-19]);
        for _ in 0..3 {
            fx.carried_pass(k, &p, "old");
        }
        let failure = if idle {
            fx.carried_idle(&shallow, "old")
        } else {
            fx.carried_fail(k, &shallow, "old")
        };
        let boundary = fx.tick();
        let window = Window::LiveOrCarried {
            since: Some(boundary),
        };
        assert_eq!(fx.ev.count(k, &p, window, needed(3)).unwrap().valid, 0);
        assert_eq!(
            fx.ev.failing_seq(k, &p, Some(boundary), needed(3)).unwrap(),
            Some(failure)
        );
        for done in 1..=3 {
            fx.pass(k, &p);
            let failing = fx
                .ev
                .failing_seq(k, &p, Some(boundary), needed(3))
                .unwrap()
                .is_some();
            assert_eq!(failing, done < 3, "after {done} live passes");
        }
        assert_eq!(fx.ev.count(k, &p, window, needed(3)).unwrap().valid, 3);
    }
}

#[test]
fn reset_clears_carried_core_evidence() {
    for failure in [false, true] {
        let mut fx = Fx::new(&[0, 1]);
        let mut kept = Vec::new();
        for id in [0_u8, 1] {
            let k = class(Regime::R1, 0, &[id], 60);
            let mut values = [0, 0];
            values[usize::from(id)] = -20;
            let p = fx.p(&values);
            let seqs: Vec<Seq> = (0..3)
                .map(|_| {
                    if failure {
                        fx.carried_fail(k, &p, "old")
                    } else {
                        fx.carried_pass(k, &p, "old")
                    }
                })
                .collect();
            kept.push((k, p, seqs));
        }
        fx.ev.reset_core(core(0));
        for (id, (k, p, seqs)) in kept.iter().enumerate() {
            let present = if failure {
                fx.ev.failing_seq(*k, p, None, needed(3)).unwrap().is_some()
            } else {
                fx.valid(*k, p) > 0
            };
            assert_eq!(present, id == 1, "core {id} after resetting core 0");
            for s in seqs {
                assert_eq!(fx.ev.carried_source(*s).is_some(), id == 1);
            }
        }
    }
}

#[test]
fn reset_clears_carried_idle_failure() {
    let mut fx = Fx::new(&[0, 1]);
    let p = fx.p(&[-20, -20]);
    let failure = fx.carried_idle(&p, "old");
    let k = class(Regime::R6, 0, &[0, 1], 900);
    let later = Some(seq(failure.get() + 1));
    assert!(
        fx.ev
            .failing_seq(k, &p, later, needed(1))
            .unwrap()
            .is_some()
    );
    fx.ev.reset_core(core(0));
    assert_eq!(fx.ev.failing_seq(k, &p, None, needed(1)).unwrap(), None);
    assert!(fx.ev.carried_source(failure).is_none());
}

#[test]
fn reset_drops_idle_failures_live_or_carried_for_any_core() {
    let mut fx = Fx::new(&[0, 1]);
    let p = fx.p(&[-20, -20]);
    fx.idle(&p);
    fx.carried_idle(&p, "old");
    fx.ev.reset_core(core(1));
    let k = class(Regime::R6, 0, &[0, 1], 900);
    assert_eq!(fx.ev.newest_failure(k, &p, None).unwrap(), None);
}

#[test]
fn reset_discards_multi_r7_facts_naming_the_core_even_when_unloaded() {
    let mut fx = Fx::new(&[0, 1, 2]);
    let k = class(Regime::R7, 0, &[0, 1], 60);
    let p = fx.p(&[-5, -5, 0]);
    let named = |fx: &mut Fx, who: Option<u8>, pass: bool| {
        let at = fx.tick();
        let obs = Observation {
            seq: at,
            class: k,
            profile: &p,
            condition: Condition::Together,
            named: who.map(core),
        };
        if pass {
            let backend = fx.ev.active_backend(k.workload().backend());
            fx.ev.record_pass(obs, backend).unwrap();
        } else {
            fx.ev.record_failure(obs).unwrap();
        }
        at
    };
    let gone_pass = named(&mut fx, Some(2), true);
    let gone_failure = named(&mut fx, Some(2), false);
    let kept_pass = named(&mut fx, Some(0), true);
    let unnamed_pass = named(&mut fx, None, true);
    fx.ev.set_checking_profile(p).unwrap();
    fx.ev.reset_core(core(2));
    let count = fx.ev.count(k, &p, ANY, needed(4)).unwrap();
    assert!(!count.witnesses.contains(&gone_pass));
    assert_eq!(
        fx.ev.newest_failure(k, &p, None).unwrap(),
        None,
        "{gone_failure:?}"
    );
    assert_eq!(count.witnesses, vec![kept_pass, unnamed_pass]);
    fx.ev.reset_core(core(0));
    assert_eq!(fx.valid(k, &p), 0, "loaded core 0 drops the whole class");
}

#[test]
fn carried_passes_do_not_add_session_exposure() {
    for condition in [Condition::Together, Condition::Parked] {
        let mut fx = Fx::new(&[0]);
        let k = class(Regime::R1, 0, &[0], 60);
        let p = fx.p(&[-10]);
        fx.carried_pass(k, &p, "old");
        fx.ev.set_checking_profile(p).unwrap();
        let boundary = fx.tick();
        let window = Window::LiveOrCarried {
            since: Some(boundary),
        };
        assert_eq!(fx.ev.count(k, &p, window, needed(1)).unwrap().valid, 1);
        assert_eq!(fx.ev.exposures().count(), 0);
        fx.pass_as(k, &p, condition);
        assert_eq!(fx.ev.exposures().collect::<Vec<_>>(), [(k.workload(), 1)]);
    }
}

#[test]
fn exposure_counts_live_non_alone_active_backend_passes_after_the_latest_failure() {
    let mut fx = Fx::new(&[0, 1]);
    let k = class(Regime::R1, 0, &[0], 60);
    let deep = fx.p(&[-12, -12]);
    let checking = fx.p(&[-10, -10]);
    fx.ev.set_checking_profile(checking).unwrap();
    fx.pass_as(k, &deep, Condition::Alone);
    fx.pass(k, &deep);
    fx.pass(k, &fx.p(&[-5, -5]));
    assert_eq!(
        fx.ev.exposure(k.workload()),
        1,
        "alone and shallower passes do not count"
    );
    fx.pass(k, &checking);
    assert_eq!(fx.ev.exposure(k.workload()), 2);
    fx.fail(k, &fx.p(&[-20, -20]));
    assert_eq!(
        fx.ev.exposure(k.workload()),
        2,
        "a failure deeper than the checking profile does not rule it out"
    );
    fx.fail(k, &fx.p(&[-10, -10]));
    assert_eq!(fx.ev.exposure(k.workload()), 0);
    fx.pass(k, &deep);
    assert_eq!(fx.ev.exposure(k.workload()), 1);
    fx.ev
        .set_backend(Backend::Mprime, Some(Path::new(MPRIME_NEW)));
    assert_eq!(fx.ev.exposure(k.workload()), 0);
    fx.ev.set_backend(Backend::Mprime, None);
    assert_eq!(fx.ev.exposure(k.workload()), 1);
}

#[test]
fn idle_failure_resets_exposure_of_all_core_r6_classes() {
    let mut fx = Fx::new(&[0, 1]);
    let r6 = class(Regime::R6, 0, &[0, 1], 60);
    let r1 = class(Regime::R1, 0, &[0], 60);
    let p = fx.p(&[-10, -10]);
    fx.ev.set_checking_profile(p).unwrap();
    fx.pass(r6, &p);
    fx.pass(r1, &p);
    fx.idle(&p);
    assert_eq!(fx.ev.exposure(r6.workload()), 0);
    assert_eq!(fx.ev.exposure(r1.workload()), 1);
}

#[test]
fn carried_failure_survives_newer_pre_boundary_live_failure() {
    for idle in [false, true] {
        let mut fx = Fx::new(&[0, 1]);
        let k = class(Regime::R6, 0, &[0, 1], 60);
        let carried_at = fx.p(&[-9, -4]);
        let carried = if idle {
            fx.carried_idle(&carried_at, "old")
        } else {
            fx.carried_fail(k, &carried_at, "old")
        };
        fx.fail(k, &fx.p(&[-10, -5]));
        fx.fail(k, &fx.p(&[-10, -10]));
        let start = fx.tick();
        let group = fx.p(&[-10, -5]);
        assert_eq!(
            fx.ev
                .failing_seq(k, &group, Some(start), needed(3))
                .unwrap(),
            Some(carried)
        );
        let live = fx.p(&[-10, -10]);
        for done in 1..=3 {
            fx.pass(k, &live);
            let want = (done < 3).then_some(carried);
            assert_eq!(
                fx.ev
                    .failing_seq(k, &group, Some(start), needed(3))
                    .unwrap(),
                want,
                "idle={idle} after {done} newer passes"
            );
        }
    }
}

#[test]
fn windows_floor_live_passes_and_let_carried_ones_through() {
    let mut fx = Fx::new(&[0]);
    let k = class(Regime::R1, 0, &[0], 60);
    let p = fx.p(&[-10]);
    let carried = fx.carried_pass(k, &p, "old");
    let early = fx.pass(k, &p);
    let boundary = fx.tick();
    let late = fx.pass(k, &p);
    let since = Some(boundary);
    let either = fx
        .ev
        .count(k, &p, Window::LiveOrCarried { since }, needed(5))
        .unwrap();
    assert_eq!(either.witnesses, vec![carried, late]);
    let live = fx
        .ev
        .count(k, &p, Window::Live { since }, needed(5))
        .unwrap();
    assert_eq!(live.witnesses, vec![late]);
    let all = fx.ev.count(k, &p, ANY, needed(2)).unwrap();
    assert_eq!((all.valid, all.witnesses), (3, vec![carried, early]));
    fx.fail(k, &p);
    assert_eq!(
        fx.ev
            .count(k, &p, Window::Live { since }, needed(5))
            .unwrap()
            .valid,
        0
    );
    assert_eq!(
        fx.ev
            .count(k, &p, Window::LiveOrCarried { since }, needed(5))
            .unwrap()
            .valid,
        0
    );
}

#[test]
fn witnesses_merge_cells_in_seq_order() {
    let mut fx = Fx::new(&[0]);
    let k = class(Regime::R1, 0, &[0], 60);
    let mut seqs = Vec::new();
    for value in [-10, -12, -10, -14, -12] {
        seqs.push(fx.pass(k, &fx.p(&[value])));
    }
    let count = fx.ev.count(k, &fx.p(&[-9]), ANY, needed(4)).unwrap();
    assert_eq!(count.valid, 5);
    assert_eq!(count.witnesses, seqs[..4]);
}

#[test]
fn class_key_identity_is_regime_workload_loaded_and_duration() {
    let a = class(Regime::R1, 0, &[0], 60);
    assert_eq!(a, class(Regime::R1, 0, &[0], 60));
    assert_ne!(a, class(Regime::R1, 1, &[0], 60));
    assert_ne!(a, class(Regime::R1, 0, &[1], 60));
    assert_ne!(a, class(Regime::R1, 0, &[0, 1], 60));
    assert_ne!(a, class(Regime::R1, 0, &[0], 61));
    assert_eq!(a.regime(), Regime::R1);
    assert!(class(Regime::R7, 0, &[0, 1], 60).multi_r7());
    assert!(!class(Regime::R7, 0, &[0], 60).multi_r7());
    assert!(!class(Regime::R6, 0, &[0, 1], 60).multi_r7());
    assert!(a < class(Regime::R1, 0, &[1], 60));
}

#[test]
fn workload_ids_round_trip_the_catalog() {
    for regime in REGIMES {
        for (index, workload) in workloads(regime).iter().enumerate() {
            let id = WorkloadId::new(regime, index).unwrap();
            assert_eq!(WorkloadId::by_id(workload.id), Some(id));
            assert_eq!(id.workload().id, workload.id);
            assert_eq!(id.regime(), regime);
            assert_eq!(id.backend(), workload.backend);
        }
        assert!(WorkloadId::new(regime, workloads(regime).len()).is_none());
    }
    assert!(WorkloadId::by_id("no-such-workload").is_none());
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "evidence seqs must ascend")]
fn seqs_must_ascend() {
    let mut fx = Fx::new(&[0]);
    let p = fx.p(&[-1]);
    let k = class(Regime::R1, 0, &[0], 60);
    fx.pass(k, &p);
    fx.next = 0;
    fx.pass(k, &p);
}

#[test]
fn repeat_volume_does_not_change_how_many_cells_a_query_visits() {
    let visits = |passes: usize| {
        let mut fx = Fx::new(&[0, 1]);
        let k = class(Regime::R1, 0, &[0], 60);
        let p = fx.p(&[-10, -10]);
        fx.ev.set_checking_profile(p).unwrap();
        for _ in 0..passes {
            fx.pass(k, &p);
        }
        fx.ev.take_visits();
        let count = fx.ev.count(k, &p, ANY, needed(3)).unwrap();
        assert_eq!(count.valid, u32::try_from(passes).unwrap());
        let counted = fx.ev.take_visits();
        fx.ev.failing_seq(k, &p, None, needed(3)).unwrap();
        fx.ev.monotonicity(k, &p, needed(3)).unwrap();
        let asked = fx.ev.take_visits();
        fx.pass(k, &p);
        assert_eq!(fx.ev.take_visits(), 0, "recording a pass visits no cell");
        (counted, asked)
    };
    let small = visits(10);
    assert_eq!(small.0, 1);
    assert_eq!(visits(10_000), small);
}

#[test]
fn visits_scale_with_distinct_cells() {
    let mut fx = Fx::new(&[0, 1]);
    let k = class(Regime::R1, 0, &[0], 60);
    for depth in 0..8_i64 {
        fx.pass(k, &fx.p(&[-depth, 0]));
    }
    fx.ev.take_visits();
    fx.ev.count(k, &fx.p(&[0, 0]), ANY, needed(1)).unwrap();
    assert_eq!(fx.ev.take_visits(), 8);
}

#[test]
fn backend_and_profile_changes_visit_only_classes_that_can_have_exposure() {
    let visits = |unrelated: u32| {
        let mut fx = Fx::new(&[0, 1]);
        let own = class(Regime::R1, 0, &[0], 60);
        let backend = own.workload().backend();
        let checking = fx.p(&[-10, -10]);
        fx.ev.set_checking_profile(checking).unwrap();
        let shallow = fx.p(&[-5, -5]);
        for secs in 0..unrelated {
            fx.pass(class(Regime::R1, 0, &[0], 1000 + secs), &shallow);
        }
        fx.pass(own, &checking);
        assert_eq!(fx.ev.exposure(own.workload()), 1);
        fx.ev.take_visits();
        assert!(!fx.ev.set_backend(backend, None));
        let unchanged = fx.ev.take_visits();
        let new = match backend {
            Backend::Mprime => MPRIME_NEW,
            Backend::Ycruncher => YCRUNCHER_NEW,
        };
        assert!(fx.ev.set_backend(backend, Some(Path::new(new))));
        let switched = fx.ev.take_visits();
        assert_eq!(fx.ev.exposure(own.workload()), 0);
        assert!(!fx.ev.set_backend(backend, Some(Path::new(new))));
        assert_eq!(fx.ev.take_visits(), 0);
        fx.ev.set_backend(backend, None);
        fx.ev.take_visits();
        fx.ev.set_checking_profile(fx.p(&[-12, -12])).unwrap();
        let moved = fx.ev.take_visits();
        let [maintained, rebuilt] = fx.ev.debug_rebuild();
        assert_eq!(maintained, rebuilt);
        (unchanged, switched, moved)
    };
    let small = visits(100);
    assert_eq!(small.0, 0, "an unchanged backend path rebuilds no exposure");
    assert!(
        small.1 > 0 && small.2 > 0,
        "the class that can have exposure is recounted"
    );
    assert_eq!(visits(10_000), small);
}

#[derive(Clone, Debug)]
enum RawOp {
    Pass {
        class: usize,
        p: Vec<i64>,
        alone: bool,
        named: Option<u8>,
    },
    Fail {
        class: usize,
        p: Vec<i64>,
        named: Option<u8>,
    },
    Idle {
        p: Vec<i64>,
        carried: bool,
    },
    CarriedPass {
        class: usize,
        p: Vec<i64>,
        path: Option<usize>,
        named: Option<u8>,
    },
    CarriedFail {
        class: usize,
        p: Vec<i64>,
        named: Option<u8>,
    },
    Reset(u8),
    Backend {
        ycruncher: bool,
        path: Option<usize>,
    },
    Checking(Vec<i64>),
}

fn values() -> impl Strategy<Value = Vec<i64>> {
    prop::collection::vec(-3_i64..=0, 4)
}

fn named() -> impl Strategy<Value = Option<u8>> {
    prop::option::of(0_u8..4)
}

fn path() -> impl Strategy<Value = Option<usize>> {
    prop::option::of(0_usize..2)
}

fn ops() -> impl Strategy<Value = RawOp> {
    prop_oneof![
        6 => (0..5_usize, values(), any::<bool>(), named()).prop_map(|(class, p, alone, named)| RawOp::Pass { class, p, alone, named }),
        3 => (0..5_usize, values(), named()).prop_map(|(class, p, named)| RawOp::Fail { class, p, named }),
        1 => (values(), any::<bool>()).prop_map(|(p, carried)| RawOp::Idle { p, carried }),
        3 => (0..5_usize, values(), path(), named()).prop_map(|(class, p, path, named)| RawOp::CarriedPass { class, p, path, named }),
        2 => (0..5_usize, values(), named()).prop_map(|(class, p, named)| RawOp::CarriedFail { class, p, named }),
        1 => (0_u8..4).prop_map(RawOp::Reset),
        1 => (any::<bool>(), path()).prop_map(|(ycruncher, path)| RawOp::Backend { ycruncher, path }),
        1 => values().prop_map(RawOp::Checking),
    ]
}

#[derive(Clone, Debug)]
struct Query {
    class: usize,
    p: Vec<i64>,
    live_only: bool,
    since: u64,
    needed: u16,
}

fn queries() -> impl Strategy<Value = Query> {
    (0..5_usize, values(), any::<bool>(), 0_u64..70, 1_u16..4).prop_map(
        |(class, p, live_only, since, needed)| Query {
            class,
            p,
            live_only,
            since,
            needed,
        },
    )
}

#[derive(Clone, Debug)]
struct Scenario {
    cores: usize,
    pool: Vec<(usize, usize, u8, u32)>,
    ops: Vec<RawOp>,
    queries: Vec<Query>,
}

fn scenarios() -> impl Strategy<Value = Scenario> {
    (
        2_usize..=4,
        prop::collection::vec((0..3_usize, 0..3_usize, any::<u8>(), 1_u32..=2), 2),
        prop::collection::vec(ops(), 1..60),
        prop::collection::vec(queries(), 40),
    )
        .prop_map(|(cores, pool, ops, queries)| Scenario {
            cores,
            pool,
            ops,
            queries,
        })
}

const PATHS: [&str; 2] = ["/nix/store/a", "/nix/store/b"];

fn path_of(index: Option<usize>) -> Option<&'static Path> {
    index.map(|i| Path::new(PATHS[i]))
}

#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the oracle keeps each fact flag plain"
)]
struct Entry {
    seq: Seq,
    class: ClassKey,
    profile: Profile,
    pass: bool,
    carried: bool,
    path: Option<usize>,
    alone: bool,
    named: Option<CoreId>,
    idle: bool,
}

struct Oracle {
    all: CoreSet,
    entries: Vec<Entry>,
    active: [Option<usize>; 2],
    checking: Option<Profile>,
}

const fn slot(backend: Backend) -> usize {
    match backend {
        Backend::Mprime => 0,
        Backend::Ycruncher => 1,
    }
}

impl Oracle {
    fn all_core_r6(&self, class: ClassKey) -> bool {
        class.regime() == Regime::R6 && class.loaded() == self.all
    }

    fn failure_before(&self, class: ClassKey, p: &Profile, since: Option<Seq>) -> Option<Seq> {
        self.entries
            .iter()
            .filter(|e| {
                !e.pass
                    && (if e.idle {
                        self.all_core_r6(class)
                    } else {
                        e.class == class
                    })
                    && e.profile.within(p)
                    && (e.carried || Some(e.seq) > since)
            })
            .map(|e| e.seq)
            .max()
    }

    fn passes(&self, class: ClassKey, p: &Profile, window: Window) -> Vec<Seq> {
        let floor = self.failure_before(class, p, None);
        self.entries
            .iter()
            .filter(|e| {
                e.pass
                    && !e.idle
                    && e.class == class
                    && e.path == self.active[slot(class.workload().backend())]
                    && e.profile.covers(p)
                    && Some(e.seq) > floor
                    && match window {
                        Window::Live { since } => !e.carried && Some(e.seq) > since,
                        Window::LiveOrCarried { since } => e.carried || Some(e.seq) > since,
                    }
            })
            .map(|e| e.seq)
            .collect()
    }

    fn failing_seq(
        &self,
        class: ClassKey,
        p: &Profile,
        since: Option<Seq>,
        n: usize,
    ) -> Option<Seq> {
        if self.passes(class, p, ANY).len() >= n {
            None
        } else {
            self.failure_before(class, p, since)
        }
    }

    fn monotonicity(&self, class: ClassKey, p: &Profile, n: usize) -> Option<Vec<Seq>> {
        let passes = self.passes(class, p, ANY);
        (!class.multi_r7() && passes.len() >= n).then(|| passes[..n].to_vec())
    }

    fn idle_monotonicity(&self, p: &Profile, n: usize) -> Option<IdleContradiction> {
        let mut classes: Vec<ClassKey> = self
            .entries
            .iter()
            .filter(|e| !e.idle && self.all_core_r6(e.class))
            .map(|e| e.class)
            .collect();
        classes.sort();
        classes.dedup();
        classes
            .into_iter()
            .filter_map(|class| {
                let passes = self.passes(class, p, ANY);
                (passes.len() >= n).then(|| IdleContradiction {
                    class,
                    passes: passes[..n].to_vec(),
                })
            })
            .min_by_key(|found| found.passes[0])
    }

    fn exposure(&self, workload: WorkloadId) -> u32 {
        let Some(checking) = self.checking else {
            return 0;
        };
        let mut classes: Vec<ClassKey> = self
            .entries
            .iter()
            .filter(|e| !e.idle && e.class.workload() == workload)
            .map(|e| e.class)
            .collect();
        classes.sort();
        classes.dedup();
        let total: usize = classes
            .into_iter()
            .map(|class| {
                let floor = self.failure_before(class, &checking, None);
                self.entries
                    .iter()
                    .filter(|e| {
                        e.pass
                            && !e.idle
                            && !e.carried
                            && !e.alone
                            && e.class == class
                            && e.path == self.active[slot(class.workload().backend())]
                            && e.profile.covers(&checking)
                            && Some(e.seq) > floor
                    })
                    .count()
            })
            .sum();
        u32::try_from(total).unwrap()
    }

    fn reset(&mut self, core: CoreId) {
        let all = self.all;
        self.entries.retain(|e| {
            let discard = if e.idle {
                all.contains(core)
            } else {
                e.class.loaded().contains(core) || e.class.multi_r7() && e.named == Some(core)
            };
            !discard
        });
    }
}

fn resolve_class(pool: &[ClassKey], index: usize) -> ClassKey {
    pool[index % pool.len()]
}

fn run(scenario: &Scenario) {
    let ids: Vec<u8> = (0..u8::try_from(scenario.cores).unwrap()).collect();
    let mut fx = Fx::new(&ids);
    let all = fx.topo.all();
    let mut oracle = Oracle {
        all,
        entries: Vec::new(),
        active: [None, None],
        checking: None,
    };
    let cores = scenario.cores;
    let nth = |named: Option<u8>| named.map(|n| core(n % u8::try_from(cores).unwrap()));
    let mut pool = vec![
        class(Regime::R6, 0, &ids, 1),
        class(Regime::R6, 0, &ids, 2),
        class(Regime::R7, 0, &ids, 1),
    ];
    for &(kind, index, mask, duration) in &scenario.pool {
        let regime = [Regime::R1, Regime::R6, Regime::R7][kind];
        let bits = u32::from(mask) % ((1_u32 << cores) - 1) + 1;
        let loaded: Vec<u8> = ids
            .iter()
            .copied()
            .filter(|&id| bits >> id & 1 == 1)
            .collect();
        pool.push(class(regime, index, &loaded, duration));
    }
    let workloads: Vec<WorkloadId> = pool.iter().map(|k| k.workload()).collect();
    for op in &scenario.ops {
        match op {
            RawOp::Pass {
                class: c,
                p,
                alone,
                named,
            } => {
                let (k, p) = (resolve_class(&pool, *c), fx.p(p));
                let at = fx.tick();
                let condition = if *alone {
                    Condition::Alone
                } else {
                    Condition::Together
                };
                let named = nth(*named);
                let backend = fx.ev.active_backend(k.workload().backend());
                fx.ev
                    .record_pass(
                        Observation {
                            seq: at,
                            class: k,
                            profile: &p,
                            condition,
                            named,
                        },
                        backend,
                    )
                    .unwrap();
                oracle.entries.push(Entry {
                    seq: at,
                    class: k,
                    profile: p,
                    pass: true,
                    carried: false,
                    path: oracle.active[slot(k.workload().backend())],
                    alone: *alone,
                    named,
                    idle: false,
                });
            }
            RawOp::Fail { class: c, p, named } => {
                let (k, p) = (resolve_class(&pool, *c), fx.p(p));
                let at = fx.tick();
                let named = nth(*named);
                fx.ev
                    .record_failure(Observation {
                        seq: at,
                        class: k,
                        profile: &p,
                        condition: Condition::Together,
                        named,
                    })
                    .unwrap();
                oracle.entries.push(Entry {
                    seq: at,
                    class: k,
                    profile: p,
                    pass: false,
                    carried: false,
                    path: None,
                    alone: false,
                    named,
                    idle: false,
                });
            }
            RawOp::Idle { p, carried } => {
                let p = fx.p(p);
                let at = if *carried {
                    fx.carried_idle(&p, "s")
                } else {
                    fx.idle(&p)
                };
                oracle.entries.push(Entry {
                    seq: at,
                    class: pool[0],
                    profile: p,
                    pass: false,
                    carried: *carried,
                    path: None,
                    alone: false,
                    named: None,
                    idle: true,
                });
            }
            RawOp::CarriedPass {
                class: c,
                p,
                path,
                named,
            } => {
                let (k, p) = (resolve_class(&pool, *c), fx.p(p));
                let at = fx.tick();
                let named = nth(*named);
                let backend = fx.ev.backend_id(k.workload().backend(), path_of(*path));
                fx.ev
                    .record_carried_pass(
                        Observation {
                            seq: at,
                            class: k,
                            profile: &p,
                            condition: Condition::Together,
                            named,
                        },
                        backend,
                        SessionRef::new("s"),
                    )
                    .unwrap();
                oracle.entries.push(Entry {
                    seq: at,
                    class: k,
                    profile: p,
                    pass: true,
                    carried: true,
                    path: *path,
                    alone: false,
                    named,
                    idle: false,
                });
            }
            RawOp::CarriedFail { class: c, p, named } => {
                let (k, p) = (resolve_class(&pool, *c), fx.p(p));
                let at = fx.tick();
                let named = nth(*named);
                fx.ev
                    .record_carried_failure(
                        Observation {
                            seq: at,
                            class: k,
                            profile: &p,
                            condition: Condition::Together,
                            named,
                        },
                        SessionRef::new("s"),
                    )
                    .unwrap();
                oracle.entries.push(Entry {
                    seq: at,
                    class: k,
                    profile: p,
                    pass: false,
                    carried: true,
                    path: None,
                    alone: false,
                    named,
                    idle: false,
                });
            }
            RawOp::Reset(c) => {
                let c = core(c % u8::try_from(cores).unwrap());
                fx.ev.reset_core(c);
                oracle.reset(c);
            }
            RawOp::Backend { ycruncher, path } => {
                let backend = if *ycruncher {
                    Backend::Ycruncher
                } else {
                    Backend::Mprime
                };
                fx.ev.set_backend(backend, path_of(*path));
                oracle.active[slot(backend)] = *path;
            }
            RawOp::Checking(p) => {
                let p = fx.p(p);
                fx.ev.set_checking_profile(p).unwrap();
                oracle.checking = Some(p);
            }
        }
        for &w in &workloads {
            assert_eq!(
                fx.ev.exposure(w),
                oracle.exposure(w),
                "exposure after {op:?}"
            );
        }
        let [maintained, rebuilt] = fx.ev.debug_rebuild();
        assert_eq!(
            maintained, rebuilt,
            "exposure drifted from a rebuild after {op:?}"
        );
    }
    for e in &oracle.entries {
        assert_eq!(
            fx.ev.carried_source(e.seq).is_some(),
            e.carried,
            "provenance of {:?}",
            e.seq
        );
    }
    for q in &scenario.queries {
        let (k, p) = (resolve_class(&pool, q.class), fx.p(&q.p));
        let since = Seq::new(q.since);
        let n = needed(q.needed);
        let window = if q.live_only {
            Window::Live { since }
        } else {
            Window::LiveOrCarried { since }
        };
        let want = oracle.passes(k, &p, window);
        let got = fx.ev.count(k, &p, window, n).unwrap();
        assert_eq!(got.valid as usize, want.len(), "valid {q:?}");
        assert_eq!(
            got.witnesses,
            want[..want.len().min(usize::from(q.needed))],
            "witnesses {q:?}"
        );
        assert_eq!(
            fx.ev.newest_failure(k, &p, since).unwrap(),
            oracle.failure_before(k, &p, since),
            "newest {q:?}"
        );
        assert_eq!(
            fx.ev.failing_seq(k, &p, since, n).unwrap(),
            oracle.failing_seq(k, &p, since, usize::from(q.needed)),
            "failing {q:?}"
        );
        assert_eq!(
            fx.ev.monotonicity(k, &p, n).unwrap(),
            oracle.monotonicity(k, &p, usize::from(q.needed)),
            "monotonicity {q:?}"
        );
        assert_eq!(
            fx.ev.idle_monotonicity(&p, n).unwrap(),
            oracle.idle_monotonicity(&p, usize::from(q.needed)),
            "idle {q:?}"
        );
    }
}

fn fixed_config() -> ProptestConfig {
    ProptestConfig {
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x7061_7274_6974_696f),
        failure_persistence: None,
        ..ProptestConfig::with_cases(600)
    }
}

proptest! {
    #![proptest_config(fixed_config())]

    #[test]
    fn count_equals_bruteforce_ledger(scenario in scenarios()) {
        run(&scenario);
    }
}

#[test]
fn a_failure_named_later_is_discarded_with_its_core() {
    let mut fx = Fx::new(&[0, 1, 2]);
    let k = class(Regime::R7, 0, &[0, 1], 60);
    let p = fx.p(&[-10, -10, 0]);
    let at = fx.fail(k, &p);
    assert_eq!(fx.ev.name_failure(k, &p, at, core(2)), Ok(()));
    assert_eq!(fx.ev.newest_failure(k, &p, None).unwrap(), Some(at));
    fx.ev.reset_core(core(2));
    assert_eq!(fx.ev.newest_failure(k, &p, None).unwrap(), None);

    let kept = fx.fail(k, &p);
    let other = class(Regime::R1, 0, &[0], 60);
    let single = fx.fail(other, &p);
    fx.ev.name_failure(k, &p, kept, core(0)).unwrap();
    fx.ev.name_failure(k, &p, kept, core(2)).unwrap();
    fx.ev.name_failure(other, &p, single, core(2)).unwrap();
    fx.ev.reset_core(core(2));
    assert_eq!(fx.ev.newest_failure(k, &p, None).unwrap(), Some(kept));
    assert_eq!(fx.ev.newest_failure(other, &p, None).unwrap(), Some(single));
    fx.ev.reset_core(core(0));
    assert_eq!(fx.ev.newest_failure(k, &p, None).unwrap(), None);
}

#[test]
fn reset_of_a_named_core_visits_cells_not_facts() {
    let visits = |facts: usize| {
        let mut fx = Fx::new(&[0, 1, 2]);
        let k = class(Regime::R7, 0, &[0, 1], 60);
        let loaded = class(Regime::R1, 0, &[2], 60);
        let p = fx.p(&[-10, -10, 0]);
        let mut carried = Vec::new();
        for _ in 0..facts {
            let backend = fx.ev.active_backend(k.workload().backend());
            let obs = Observation {
                seq: fx.tick(),
                class: k,
                profile: &p,
                condition: Condition::Together,
                named: Some(core(2)),
            };
            fx.ev.record_pass(obs, backend).unwrap();
            let at = fx.tick();
            fx.ev
                .record_failure(Observation { seq: at, ..obs })
                .unwrap();
            let at = fx.tick();
            fx.ev
                .record_carried_pass(
                    Observation { seq: at, ..obs },
                    backend,
                    SessionRef::new("old"),
                )
                .unwrap();
            carried.push(at);
            let at = fx.tick();
            fx.ev
                .record_carried_failure(Observation { seq: at, ..obs }, SessionRef::new("old"))
                .unwrap();
            carried.push(at);
            carried.push(fx.carried_pass(loaded, &p, "old"));
            carried.push(fx.carried_fail(loaded, &p, "old"));
            carried.push(fx.carried_idle(&p, "old"));
        }
        fx.ev.take_visits();
        fx.ev.reset_core(core(2));
        let cost = fx.ev.take_visits();
        let forgotten = carried.iter().all(|&at| fx.ev.carried_source(at).is_none());
        (
            cost,
            fx.valid(k, &p),
            fx.valid(loaded, &p),
            fx.ev.newest_failure(k, &p, None).unwrap(),
            fx.ev.newest_failure(loaded, &p, None).unwrap(),
            forgotten,
        )
    };
    let small = visits(100);
    let large = visits(10_000);
    assert_eq!(small, large);
    assert_eq!(
        (small.1, small.2, small.3, small.4, small.5),
        (0, 0, None, None, true)
    );
}

#[test]
fn the_ledger_rejects_a_profile_of_other_cores() {
    let mut fx = Fx::new(&[0, 1]);
    let other = topology(&[4, 5]);
    let foreign = profile(&other, &[-10, -10]);
    let k = class(Regime::R1, 0, &[0], 60);
    let mismatch = TopologyMismatch {
        expected: fx.topo.all(),
        found: foreign.cores(),
    };
    let at = fx.tick();
    let obs = Observation {
        seq: at,
        class: k,
        profile: &foreign,
        condition: Condition::Together,
        named: None,
    };
    let backend = fx.ev.active_backend(k.workload().backend());
    assert_eq!(fx.ev.record_pass(obs, backend), Err(mismatch));
    assert_eq!(fx.ev.record_failure(obs), Err(mismatch));
    assert_eq!(fx.ev.record_idle_failure(at, &foreign), Err(mismatch));
    assert_eq!(fx.ev.count(k, &foreign, ANY, needed(1)), Err(mismatch));
    assert_eq!(fx.ev.newest_failure(k, &foreign, None), Err(mismatch));
    assert_eq!(
        fx.ev.failing_seq(k, &foreign, None, needed(1)),
        Err(mismatch)
    );
    assert_eq!(fx.ev.monotonicity(k, &foreign, needed(1)), Err(mismatch));
    assert_eq!(fx.ev.idle_monotonicity(&foreign, needed(1)), Err(mismatch));
    assert_eq!(fx.ev.set_checking_profile(foreign), Err(mismatch));
    assert_eq!(fx.ev.take_visits(), 0);
}

#[test]
fn naming_a_failure_that_is_not_there_changes_nothing() {
    let mut fx = Fx::new(&[0, 1, 2]);
    let k = class(Regime::R7, 0, &[0, 1], 60);
    let p = fx.p(&[-10, -10, 0]);
    let elsewhere = fx.p(&[-20, -20, 0]);
    assert_eq!(fx.ev.name_failure(k, &p, seq(1), core(2)), Ok(()));
    let at = fx.fail(k, &p);
    assert_eq!(fx.ev.name_failure(k, &elsewhere, at, core(2)), Ok(()));
    assert_eq!(fx.ev.name_failure(k, &p, seq(99), core(2)), Ok(()));
    fx.ev.reset_core(core(2));
    assert_eq!(fx.ev.newest_failure(k, &p, None).unwrap(), Some(at));
}

#[test]
fn a_carried_failure_named_later_is_discarded_with_its_core_and_its_provenance() {
    let mut fx = Fx::new(&[0, 1, 2]);
    let k = class(Regime::R7, 0, &[0, 1], 60);
    let p = fx.p(&[-10, -10, 0]);
    let at = fx.carried_fail(k, &p, "old");
    assert!(fx.ev.carried_source(at).is_some());
    fx.ev.name_failure(k, &p, at, core(2)).unwrap();
    assert_eq!(fx.ev.newest_failure(k, &p, None).unwrap(), Some(at));
    fx.ev.reset_core(core(2));
    assert_eq!(fx.ev.newest_failure(k, &p, None).unwrap(), None);
    assert!(fx.ev.carried_source(at).is_none());
}
