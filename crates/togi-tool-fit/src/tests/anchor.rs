use std::cell::Cell;
use std::fs;
use std::path::Path;

use togi_facts::Decisive;
use togi_machine::{Regime, Signal, workloads};
use togi_sim::definition::{SharedVoltageDef, VoltageCoreDef, VoltageWorkloadDef, Weights};
use togi_sim::{Load, MachineDefinition, Model, file};
use togi_tool_modelcheck::{Status, check};
use togi_tool_trialfacts::{Class, Extracts, TrialFact};

use super::{context, extract, fact};
use crate::anchor::anchor_with;
use crate::error::{Error, EvidenceError};
use crate::evidence::{initial_config, load_of};

fn anchor_records() -> Vec<TrialFact> {
    (0..20)
        .map(|i| {
            let mut record = fact(
                &[-20; 16],
                Regime::R7,
                &[0, 1, 2, 3, 4, 5, 6, 7],
                120,
                if i >= 10 {
                    super::crash()
                } else {
                    Decisive::Pass
                },
            );
            if i >= 10 {
                record.duration_s = 60;
            }
            record
        })
        .collect()
}

fn idle_failure() -> TrialFact {
    let mut idle = anchor_records().remove(10);
    idle.class = Class::Idle {
        loaded: super::cores(&[0]),
    };
    idle
}

/// A shared-voltage machine whose multi-core R7 trials fail with probability exactly 1/2 at 120 s.
fn anchor_config(records: &[TrialFact]) -> (MachineDefinition, f64) {
    let mut cfg = initial_config(records);
    cfg.joint.clear();
    cfg.ccd = None;
    cfg.model.past_limit_rate = 0.0;
    cfg.model.near_limit_rate = 0.0;
    let background = 0.000_123_456_789_012_345_6;
    let mut shared = SharedVoltageDef {
        idle_v: 0.8,
        margin_v: 0.006,
        rate: (std::f64::consts::LN_2 / 120.0 - background) / 8.0,
        background_rate: background,
        power_limit_w: 192.0,
        thermal_limit_w: 192.0,
        workload: std::collections::BTreeMap::new(),
    };
    for workload in workloads(Regime::R7) {
        let core = |i: u8| VoltageCoreDef {
            base_v: 1.2,
            threshold_v: 1.2 - 0.0036 * 20.0,
            count_v: 0.0036,
            clock_v_per_100mhz: 0.033_123_456_789_012_345 + f64::from(i) * 0.0001,
            threshold_clock_v_per_100mhz: 0.012_345_678_901_234_567 + f64::from(i) * 0.0001,
            signals: Some(Weights::from([
                (Signal::Crash, 1.0),
                (Signal::ComputationError, 0.25),
            ])),
        };
        shared.workload.insert(
            workload.id.to_owned(),
            VoltageWorkloadDef {
                reference_mhz: 5240.0,
                full_mhz: [5240.0, 5240.0],
                idle_gain_mhz: f64::from(u8::try_from(workload.id.len()).unwrap()),
                watts_per_core: 13.0,
                offset_watts_per_count: 0.0,
                package_mhz_per_w: 0.0,
                balance_mhz_per_w: 0.0,
                core: (0..16_u8).map(core).collect(),
            },
        );
    }
    cfg.shared_voltage = Some(shared);
    (cfg, 20.0 * std::f64::consts::LN_2)
}

struct Fixture {
    _dir: tempfile::TempDir,
    extract: std::path::PathBuf,
    out: std::path::PathBuf,
}

fn fixture(facts: &[TrialFact]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let extract = extract(dir.path(), "facts.gz", facts);
    let out = dir.path().join("machines");
    Fixture {
        _dir: dir,
        extract,
        out,
    }
}

fn run(
    fixture: &Fixture,
    fitter: impl FnOnce(&[TrialFact]) -> Result<(MachineDefinition, f64), Error>,
) -> (Result<(), Error>, String) {
    let mut report = Vec::new();
    let result = anchor_with(&fixture.extract, &fixture.out, &mut report, fitter);
    (result, String::from_utf8(report).unwrap())
}

#[test]
fn an_anchor_is_checked_deterministic_and_replayable_from_its_own_provenance() {
    let records = anchor_records();
    let mut all = records.clone();
    all.push(idle_failure());
    let fixture = fixture(&all);
    let mut first: Option<(String, String)> = None;
    for _ in 0..2 {
        let calls = Cell::new(0);
        let (result, report) = run(&fixture, |got| {
            calls.set(calls.get() + 1);
            assert_eq!(
                got,
                records.as_slice(),
                "the fit did not receive every decisive fact"
            );
            Ok(anchor_config(got))
        });
        result.unwrap();
        assert_eq!(calls.get(), 1);
        let path = fixture.out.join("target-shared-voltage.json");
        let content = fs::read_to_string(&path).unwrap();
        let cfg = file::load(&path).unwrap();
        let (want, loss) = anchor_config(&records);
        assert_eq!(
            cfg.shared_voltage, want.shared_voltage,
            "shared parameters lost precision or order"
        );
        assert_eq!(cfg.core, want.core);
        assert_eq!(cfg.facts.as_deref(), Some("../facts.gz"));
        assert_eq!(
            cfg.bios_context.as_ref().and_then(|c| c.board.as_deref()),
            Some("fixture")
        );
        let mut extracts = Extracts::new();
        let checked = check(&path, &cfg, &mut extracts).unwrap();
        assert_eq!(
            (checked.status, checked.groups.len(), checked.idle_failures),
            (Status::Ok, 1, 1)
        );
        assert_eq!((checked.groups[0].n, checked.groups[0].k), (20, 10));
        let replay = extracts.replay(&path, &cfg).unwrap();
        let class = match records[0].class {
            Class::Trial(class) => class,
            Class::Idle { .. } => unreachable!("the fixture is a trial"),
        };
        let load: Load<'_> = load_of(&class);
        let bios = Model::new(&cfg, 0).unwrap().bios_context().clone();
        assert_eq!(bios, context());
        assert!(
            replay.has_answer(&bios, &load, records[0].profile.as_slice()),
            "the generated provenance cannot answer matching facts"
        );
        for text in [
            "IN-SAMPLE",
            "NOT forward-validated",
            "2026-10-04 #306",
            "threshold_clock_v_per_100mhz",
        ] {
            assert!(
                content.contains(text),
                "missing anchor header or parameter {text}"
            );
        }
        for text in [
            "Whole-trial bootstrap",
            "layered R7 CCD joints",
            "\"replay\":",
        ] {
            assert!(
                !content.contains(text),
                "misleading or unsupported anchor content {text}"
            );
        }
        assert!(
            report.contains(&format!("raw total in-sample log loss {loss}")),
            "{report}"
        );
        assert!(
            report.contains("R7 in-sample (record only): trials=20 observed=10 predicted="),
            "{report}"
        );
        assert!(
            report.contains("ok (1 eligible groups; 1 idle failures"),
            "{report}"
        );
        let candidate = {
            let v = want.shared_voltage.as_ref().unwrap();
            format!(
                "Shared-voltage candidate (record only): rate={}/s margin_v={} background_rate={}/s",
                v.rate, v.margin_v, v.background_rate
            )
        };
        assert!(report.contains(&candidate), "{report}");
        let r7 = report
            .split("R7 in-sample (record only): ")
            .nth(1)
            .unwrap()
            .lines()
            .next()
            .unwrap();
        let field = |name: &str| -> f64 {
            r7.split(' ')
                .find_map(|part| part.strip_prefix(name))
                .unwrap()
                .parse()
                .unwrap()
        };
        assert!((field("predicted=") - 10.0).abs() < 1e-10, "{r7}");
        assert!((field("raw_log_loss=") - loss).abs() < 1e-10, "{r7}");
        assert!(
            (field("log_loss/trial=") - std::f64::consts::LN_2).abs() < 1e-10,
            "{r7}"
        );
        match &first {
            None => first = Some((content, report)),
            Some((c, r)) => assert!(
                *c == content && *r == report,
                "fixed inputs changed the anchor or its report"
            ),
        }
        assert!(
            !fixture.out.join("target-fit-0.json").exists(),
            "anchor mode generated a legacy fit"
        );
    }
}

#[test]
fn a_flagged_anchor_prints_its_scores_and_never_writes() {
    for existing in [false, true] {
        let fixture = fixture(&anchor_records());
        let path = fixture.out.join("target-shared-voltage.json");
        if existing {
            fs::create_dir_all(&fixture.out).unwrap();
            fs::write(&path, "preserve anchor").unwrap();
        }
        let (result, report) = run(&fixture, |records| {
            let (mut cfg, loss) = anchor_config(records);
            let shared = cfg.shared_voltage.as_mut().unwrap();
            shared.background_rate = 0.0;
            for w in shared.workload.values_mut() {
                for core in &mut w.core {
                    core.threshold_v = 0.8;
                }
            }
            Ok((cfg, loss))
        });
        assert!(matches!(result, Err(Error::AnchorFlagged)), "{result:?}");
        assert!(report.contains("flagged (1 eligible groups"), "{report}");
        assert!(
            report.contains(
                "cores=0,1,2,3,4,5,6,7 duration=120s depth=-20 n=20 k=10 interval=[0,0] mean_p="
            ),
            "{report}"
        );
        assert!(report.contains(
            "All trials in-sample (record only): trials=20 raw total in-sample log loss "
        ));
        assert_eq!(report.matches("log_loss/trial=").count(), 2, "{report}");
        assert!(!report.contains("Wrote IN-SAMPLE anchor:"));
        if existing {
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                "preserve anchor",
                "the flagged fit replaced the anchor"
            );
        } else {
            assert!(
                !Path::new(&fixture.out).exists(),
                "the flagged fit created the output directory"
            );
        }
    }
}

#[test]
fn facts_the_anchor_cannot_replay_are_refused_before_any_fit() {
    type Edit = fn(&mut Vec<TrialFact>);
    type Expected = fn(&Error) -> bool;
    let cases: [(&str, Edit, Expected); 3] = [
        (
            "core count",
            |records| {
                for record in records {
                    record.profile = super::profile(&[-20, -20]);
                    if let Class::Trial(class) = &mut record.class {
                        class.loaded = super::cores(&[0, 1]);
                    }
                }
            },
            |e| matches!(e, Error::NeedsSixteenCores { cores: 2, .. }),
        ),
        (
            "missing BIOS",
            |records| {
                for record in records {
                    record.context = None;
                }
            },
            |e| matches!(e, Error::NeedsContext),
        ),
        (
            "mixed BIOS",
            |records| records[1].context.as_mut().unwrap().board = "other".into(),
            |e| matches!(e, Error::Evidence(EvidenceError::MixedContexts)),
        ),
    ];
    for (name, edit, expected) in cases {
        let mut records = anchor_records();
        edit(&mut records);
        let fixture = fixture(&records);
        let (result, report) = run(&fixture, |_| {
            panic!("invalid facts reached the shared-voltage fit")
        });
        let error = result.unwrap_err();
        assert!(expected(&error), "{name}: {error}");
        assert!(report.is_empty() && !fixture.out.exists(), "{name}");
    }
}

#[test]
fn a_fitter_that_returns_no_shared_voltage_model_is_refused() {
    let fixture = fixture(&anchor_records());
    let (result, _) = run(&fixture, |records| Ok((initial_config(records), 0.0)));
    assert!(matches!(result, Err(Error::NotSharedVoltage)));
}
