use test_case::test_case;
use togi_machine::{CcdId, CoreId, CoreInfo, CoreSet, ResetKind, ResetReason};

use crate::{CrashFacts, CrashKind, Topology, TopologyError, classify_crash, recorded_kind};

pub(crate) fn core(id: u8) -> CoreId {
    CoreId::new(i64::from(id)).unwrap()
}

fn info(id: u8, ccd: u8) -> CoreInfo {
    CoreInfo {
        core: core(id),
        ccd: CcdId::try_from(ccd).unwrap(),
        cpus: Vec::new(),
    }
}

fn topology(ids: &[u8]) -> Topology {
    let infos: Vec<CoreInfo> = ids.iter().map(|&id| info(id, id / 8)).collect();
    Topology::new(&infos).unwrap()
}

#[test]
fn topology_alternates_ccds_and_accepts_unequal_sizes() {
    let topology =
        Topology::new(&[info(9, 1), info(0, 0), info(2, 0), info(8, 1), info(1, 0)]).unwrap();
    let order: Vec<u8> = topology.order().iter().map(|core| core.get()).collect();
    assert_eq!(order, [0, 8, 1, 9, 2]);
    assert_eq!(topology.ccd_of(core(9)), Some(CcdId::new(1).unwrap()));
    assert_eq!(topology.ccd_of(core(3)), None);
    let ccds: Vec<(u8, Vec<u8>)> = topology
        .ccds()
        .iter()
        .map(|(ccd, set)| (ccd.get(), set.iter().map(CoreId::get).collect()))
        .collect();
    assert_eq!(ccds, [(0, vec![0, 1, 2]), (1, vec![8, 9])]);
    assert_eq!(topology.all().len(), 5);
    let parts: Vec<usize> = topology.parts().iter().map(CoreSet::len).collect();
    assert_eq!(parts, [3, 2, 5]);
}

#[test]
fn single_ccd_topology_has_the_ccd_as_its_only_part() {
    let topology = topology(&[0, 1, 2]);
    assert_eq!(topology.parts(), [topology.all()]);
}

#[test]
fn topology_rejects_empty_duplicate_and_four_ccds() {
    assert_eq!(Topology::new(&[]), Err(TopologyError::Empty));
    assert_eq!(
        Topology::new(&[info(0, 0), info(0, 1)]),
        Err(TopologyError::DuplicateCore(core(0)))
    );
    let four: Vec<CoreInfo> = (0..4).map(|ccd| info(ccd * 2, ccd)).collect();
    assert_eq!(Topology::new(&four), Err(TopologyError::TooManyCcds(4)));
    let three: Vec<CoreInfo> = (0..3).map(|ccd| info(ccd * 2, ccd)).collect();
    assert!(Topology::new(&three).is_ok());
}

fn classify(facts: &CrashFacts) -> CrashKind {
    let kind = classify_crash(facts);
    let recorded = recorded_kind(
        kind == CrashKind::Stray,
        kind == CrashKind::Inconclusive || kind == CrashKind::Thermal,
        &facts.reason,
        facts.in_trial,
    );
    assert_eq!(recorded, kind, "recorded kind of {facts:?}");
    kind
}

fn reason(kind: Option<ResetKind>, supported: bool) -> ResetReason {
    ResetReason {
        kind,
        raw: Vec::new(),
        supported,
    }
}

fn nothing(reason: ResetReason) -> CrashFacts {
    CrashFacts {
        in_trial: false,
        applied: false,
        evidence: false,
        reason,
        confirmed: false,
    }
}

#[test_case("trial", &CrashFacts {
                in_trial: true,
                ..nothing(reason(None, false))
            }, CrashKind::InTrial; "trial")]
#[test_case("idle", &CrashFacts {
                applied: true,
                ..nothing(reason(None, false))
            }, CrashKind::Idle; "idle")]
#[test_case("stray", &nothing(reason(None, false)), CrashKind::Stray; "stray")]
#[test_case("evidence before thermal", &CrashFacts {
                applied: true,
                evidence: true,
                ..nothing(reason(Some(ResetKind::ThermalTrip), true))
            }, CrashKind::Idle; "evidence before thermal")]
#[test_case("thermal", &CrashFacts {
                in_trial: true,
                ..nothing(reason(Some(ResetKind::ThermalTrip), true))
            }, CrashKind::Thermal; "thermal")]
#[test_case("power button trial", &CrashFacts {
                in_trial: true,
                ..nothing(reason(Some(ResetKind::PowerButton), true))
            }, CrashKind::InTrial; "power button trial")]
#[test_case("power button idle", &CrashFacts {
                applied: true,
                ..nothing(reason(Some(ResetKind::PowerButton), true))
            }, CrashKind::Inconclusive; "power button idle")]
#[test_case("confirmed absent reason", &CrashFacts {
                applied: true,
                confirmed: true,
                ..nothing(reason(None, true))
            }, CrashKind::Inconclusive; "confirmed absent reason")]
#[test_case("unsupported absent reason", &CrashFacts {
                applied: true,
                confirmed: true,
                ..nothing(reason(None, false))
            }, CrashKind::Idle; "unsupported absent reason")]
#[test_case("unconfirmed absent reason", &CrashFacts {
                applied: true,
                ..nothing(reason(None, true))
            }, CrashKind::Idle; "unconfirmed absent reason")]
#[test_case("unknown", &nothing(reason(Some(ResetKind::Unknown), true)), CrashKind::Stray; "unknown")]
fn classify_crash_cases(name: &str, facts: &CrashFacts, want: CrashKind) {
    assert_eq!(classify(facts), want, "{name}");
}

#[test_case(ResetKind::Watchdog; "ResetKind::Watchdog")]
#[test_case(ResetKind::SyncFlood; "ResetKind::SyncFlood")]
#[test_case(ResetKind::CpuShutdown; "ResetKind::CpuShutdown")]
#[test_case(ResetKind::PowerLoss; "ResetKind::PowerLoss")]
fn other_reset_kinds_classify_by_what_was_applied(kind: ResetKind) {
    let confirmed = CrashFacts {
        confirmed: true,
        ..nothing(reason(Some(kind), true))
    };
    assert_eq!(
        classify(&CrashFacts {
            in_trial: true,
            ..confirmed.clone()
        }),
        CrashKind::InTrial
    );
    assert_eq!(
        classify(&CrashFacts {
            applied: true,
            ..confirmed.clone()
        }),
        CrashKind::Idle
    );
    assert_eq!(classify(&confirmed), CrashKind::Stray);
}

#[test]
fn thermal_reason_yields_to_mce_evidence() {
    let thermal = reason(Some(ResetKind::ThermalTrip), true);
    let with_evidence = CrashFacts {
        evidence: true,
        ..nothing(thermal.clone())
    };
    assert_eq!(
        classify(&CrashFacts {
            in_trial: true,
            applied: true,
            ..with_evidence.clone()
        }),
        CrashKind::InTrial
    );
    assert_eq!(
        classify(&CrashFacts {
            applied: true,
            ..with_evidence
        }),
        CrashKind::Idle
    );
    assert_eq!(
        recorded_kind(false, false, &thermal, true),
        CrashKind::InTrial
    );
    assert_eq!(
        recorded_kind(false, true, &thermal, true),
        CrashKind::Thermal
    );
}
