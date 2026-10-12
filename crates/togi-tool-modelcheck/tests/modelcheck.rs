#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use togi_facts::{Decisive, TrialClass};
use togi_journal::{SCHEMA, Seq};
use togi_machine::{
    BiosContext, Condition, CoreId, CoreSet, PerCore, Regime, ResetKind, Signal, TargetOffset,
    WorkloadId,
};
use togi_sim::{MachineDefinition, Model, file};
use togi_tool_modelcheck::{
    Check, Checker, Error, FactProblem, Interval, MIN_GROUP_TRIALS, Status, check, check_records,
    report,
};
use togi_tool_trialfacts::{
    BuildStamp, Class, Extracts, ExtractsError, Phase, Profile, ReadError, TrialFact, write,
};

const SESSION: &str = "20260101T000000Z";

fn definition(value: &Value) -> MachineDefinition {
    file::decode(&value.to_string()).unwrap()
}

fn model(value: &Value) -> Model {
    Model::new(&definition(value), 0).unwrap()
}

fn cores(ids: &[i64]) -> CoreSet {
    ids.iter().map(|&id| CoreId::new(id).unwrap()).collect()
}

fn workload() -> WorkloadId {
    WorkloadId::new(Regime::R1, 0).unwrap()
}

fn crash() -> Decisive {
    Decisive::Failure {
        signal: Signal::Crash,
        core: None,
        stalled: None,
    }
}

fn context(board: &str) -> BiosContext {
    BiosContext {
        bios_version: "v1".into(),
        board: board.into(),
        cpu_model: "cpu".into(),
        microcode: "0x1".into(),
        boost_limit_mhz: 5000,
    }
}

fn fact(
    seq: u64,
    profile: &[i64],
    loaded: &[i64],
    duration_s: u32,
    outcome: Decisive,
) -> TrialFact {
    let offsets: Vec<_> = profile
        .iter()
        .map(|&o| TargetOffset::new(o).unwrap())
        .collect();
    TrialFact {
        session: SESSION.parse().unwrap(),
        build: BuildStamp {
            version: "fixture".into(),
            rev: "abc".into(),
            ruleset: 1,
            schema: SCHEMA,
            fixes: 0,
            evidence: Some(1),
        },
        context: Some(context("fixture")),
        seq: Seq::new(seq).unwrap(),
        trial: None,
        class: Class::Trial(TrialClass {
            workload: workload(),
            loaded: cores(loaded),
            duration_s,
        }),
        condition: Condition::Alone,
        phase: Some(Phase::Search),
        record_only: false,
        profile: Profile::new(&offsets).unwrap(),
        outcome,
        duration_s,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
    }
}

fn passes(count: u64) -> Vec<TrialFact> {
    (1..=count)
        .map(|seq| fact(seq, &[-10, 0], &[0], 90, Decisive::Pass))
        .collect()
}

fn idle(seq: u64) -> TrialFact {
    let mut idle = fact(seq, &[-10, 0], &[0], 0, crash());
    idle.class = Class::Idle {
        loaded: cores(&[0]),
    };
    idle
}

fn limits(flat: f64) -> Value {
    let core = |id| json!({"id": id, "alone": vec![-50; 5], "together": vec![-50; 7], "flat": if id == 0 { flat } else { 0.0 }});
    json!([core(0), core(1)])
}

fn machine(near_limit_rate: f64) -> Value {
    json!({
        "cores": 2,
        "facts": "facts.jsonl.gz",
        "model": {"near_limit_rate": near_limit_rate},
        "core": limits(0.0),
    })
}

fn write_fixture(dir: &Path, machine: &Value) -> PathBuf {
    let path = dir.join("machine.json");
    std::fs::write(&path, machine.to_string()).unwrap();
    write(dir.join("facts.jsonl.gz"), &passes(10)).unwrap();
    path
}

fn check_file(path: &Path) -> Result<Check, Error> {
    check(path, &file::load(path).unwrap(), &mut Extracts::new())
}

#[test]
fn a_fixture_machine_is_judged_against_its_own_extract() {
    for (near_limit_rate, status, interval) in [
        (0.0, Status::Ok, Interval { low: 0, high: 0 }),
        (1.0, Status::Flagged, Interval { low: 10, high: 10 }),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), &machine(near_limit_rate));
        let result = check_file(&path).unwrap();
        assert_eq!(result.status, status);
        assert_eq!(result.machine, path);
        assert_eq!(result.extract, dir.path().join("facts.jsonl.gz"));
        let [group] = result.groups.as_slice() else {
            panic!("{result:?}");
        };
        assert_eq!((group.n, group.k, group.interval), (10, 0, interval));
        let mut out = Vec::new();
        report(&mut out, &[result]).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains(&format!("{}: {status}", path.display())),
            "{out}"
        );
    }
}

#[test]
fn the_report_names_each_flagged_group_and_nothing_for_no_checks() {
    let mut out = Vec::new();
    report(&mut out, &[]).unwrap();
    assert!(out.is_empty());

    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &machine(1.0));
    let flagged = check_file(&path).unwrap();
    let ok_dir = tempfile::tempdir().unwrap();
    let ok = check_file(&write_fixture(ok_dir.path(), &machine(0.0))).unwrap();
    report(&mut out, &[flagged.clone(), ok]).unwrap();
    let out = String::from_utf8(out).unwrap();
    let mean_p = flagged.groups[0].mean_p;
    assert!(mean_p > 0.99, "{mean_p}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines[0], "");
    assert_eq!(lines[1], "Model check (99% binomial intervals)");
    assert_eq!(
        lines[2],
        format!(
            "{}: flagged (1 eligible groups; 0 idle failures without trial exposure)",
            path.display()
        )
    );
    assert_eq!(
        lines[3],
        format!(
            "  R1 {} cores=0 duration=90s depth=-10 n=10 k=0 interval=[10,10] mean_p={mean_p:.6e}",
            workload()
        )
    );
    assert_eq!(lines.len(), 5, "{out}");
    assert!(lines[4].ends_with(": ok (1 eligible groups; 0 idle failures without trial exposure)"));
}

#[test]
fn a_check_serializes_with_the_class_flattened_and_the_status_in_words() {
    let dir = tempfile::tempdir().unwrap();
    let result = check_file(&write_fixture(dir.path(), &machine(0.0))).unwrap();
    let mut json = serde_json::to_value(&result).unwrap();
    json["machine"] = json!("machine.json");
    json["extract"] = json!("facts.jsonl.gz");
    assert_eq!(
        json,
        json!({
            "machine": "machine.json",
            "extract": "facts.jsonl.gz",
            "status": "ok",
            "idle_failures": 0,
            "groups": [{
                "context": {
                    "bios_version": "v1",
                    "board": "fixture",
                    "cpu_model": "cpu",
                    "microcode": "0x1",
                    "boost_limit_mhz": 5000,
                },
                "class": {
                    "regime": "R1",
                    "workload": workload().as_str(),
                    "cores": [0],
                    "duration_s": 90,
                },
                "depth": -10,
                "n": 10,
                "k": 0,
                "interval": {"low": 0, "high": 0},
                "mean_p": 0.0,
                "flagged": false,
            }],
        })
    );
}

#[test]
fn a_group_is_keyed_by_the_shallowest_loaded_offset_and_idle_cores_do_not_count() {
    let four = model(&json!({"cores": 4}));
    for (name, loaded, profile, depth) in [
        ("alone", &[1][..], [0, -30, 0, -45], -30),
        ("together", &[0, 1, 2][..], [-40, -20, -30, 0], -20),
        ("any-zero", &[0, 1, 2][..], [-40, 0, -30, 0], 0),
    ] {
        let records: Vec<_> = (1..=10)
            .map(|seq| fact(seq, &profile, loaded, 60, Decisive::Pass))
            .collect();
        let groups = Checker::new(&four, &records)
            .unwrap()
            .groups(&four)
            .unwrap();
        let [group] = groups.as_slice() else {
            panic!("{name}: {groups:?}");
        };
        assert_eq!(group.depth.get(), depth, "{name}");
    }
}

#[test]
fn trials_of_other_depths_make_other_groups_each_needing_ten() {
    let two = model(&machine(0.0));
    let mut records = passes(10);
    records.extend((11..=19).map(|seq| fact(seq, &[-20, 0], &[0], 90, Decisive::Pass)));
    let groups = Checker::new(&two, &records).unwrap().groups(&two).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].depth.get(), -10);
}

#[test]
fn groups_come_out_in_one_order_whatever_the_order_of_the_facts() {
    let two = model(&machine(0.0));
    let mut records = passes(10);
    records.extend((11..=20).map(|seq| fact(seq, &[-20, 0], &[0], 90, Decisive::Pass)));
    records.extend((21..=30).map(|seq| fact(seq, &[-5, -5], &[0, 1], 60, Decisive::Pass)));
    let forward = Checker::new(&two, &records).unwrap().groups(&two).unwrap();
    records.reverse();
    let backward = Checker::new(&two, &records).unwrap().groups(&two).unwrap();
    assert_eq!(forward.len(), 3);
    assert_eq!(forward, backward);
}

#[test]
fn a_checker_accepts_exactly_what_its_report_calls_ok() {
    let flat = |p: f64| json!({"cores": 2, "core": limits(-(-p).ln_1p() / 90.0)});
    let mut records: Vec<_> = passes(20);
    for record in &mut records[..2] {
        record.outcome = crash();
    }
    let checker = Checker::new(&model(&flat(0.1)), &records).unwrap();
    for (p, want) in [(0.0, false), (0.001, false), (0.1, true), (0.9, false)] {
        let model = model(&flat(p));
        let result = checker.check("synthetic", "synthetic", &model).unwrap();
        assert_eq!(checker.accepts(&model).unwrap(), want, "p={p}");
        assert_eq!(result.status == Status::Ok, want, "p={p}: {result:?}");
    }
}

#[test]
fn a_machine_with_an_indecisive_reset_is_refused_and_a_zero_weight_changes_nothing() {
    for kind in [ResetKind::ThermalTrip, ResetKind::PowerLoss] {
        for (name, weight) in [("zero", 0.0), ("small-positive", 1e-9), ("positive", 1.0)] {
            let dir = tempfile::tempdir().unwrap();
            let mut machine = machine(0.0);
            machine["model"]["reset"] = json!({"watchdog": 1.0, kind.as_str(): weight});
            let path = write_fixture(dir.path(), &machine);
            let result = check_file(&path);
            if weight > 0.0 {
                let Err(Error::IndecisiveReset(refused)) = result else {
                    panic!("{kind}/{name}: {result:?}");
                };
                assert_eq!(refused, kind);
                continue;
            }
            let result = result.unwrap();
            assert_eq!(result.status, Status::Ok, "{kind}/{name}");
            assert_eq!(
                result.groups.iter().map(|g| (g.n, g.k)).collect::<Vec<_>>(),
                [(10, 0)]
            );
        }
    }
}

#[test]
fn the_machines_declared_context_limits_which_facts_are_checked() {
    let declared = |board: &str| {
        let mut machine = machine(0.0);
        machine["bios_context"] = json!({
            "bios_version": "v1", "board": board, "cpu_model": "cpu", "microcode": "0x1", "boost_limit_mhz": 5000,
        });
        model(&machine)
    };
    for (name, count, recorded, machine_board, groups) in [
        ("below-ten", 9, Some("fixture"), None, 0),
        ("ten", 10, Some("fixture"), None, 1),
        ("undeclared-takes-any", 10, Some("other"), None, 1),
        ("undeclared-takes-none", 10, None, None, 1),
        ("declared-without-context", 10, None, Some("fixture"), 0),
        ("mismatch", 10, Some("fixture"), Some("other"), 0),
        ("match", 10, Some("fixture"), Some("fixture"), 1),
    ] {
        let mut records = passes(count);
        for record in &mut records {
            record.context = recorded.map(context);
        }
        let model = machine_board.map_or_else(|| model(&machine(0.0)), declared);
        let checker = Checker::new(&model, &records).unwrap();
        let groups_found = checker.groups(&model).unwrap();
        assert_eq!(groups_found.len(), groups, "{name}");
        if groups == 1 {
            assert_eq!(
                (groups_found[0].n, groups_found[0].k),
                (count.try_into().unwrap(), 0)
            );
        }
    }
    assert_eq!(MIN_GROUP_TRIALS, 10);
}

#[test]
fn an_undeclared_machine_checks_each_recorded_context_separately() {
    let two = model(&machine(0.0));
    let under = |board: &str, seqs: std::ops::RangeInclusive<u64>| -> Vec<TrialFact> {
        seqs.map(|seq| {
            let mut record = fact(seq, &[-10, 0], &[0], 90, Decisive::Pass);
            record.context = Some(context(board));
            record
        })
        .collect()
    };
    let mut records = under("first", 1..=10);
    records.extend(under("second", 11..=20));
    let forward = Checker::new(&two, &records).unwrap().groups(&two).unwrap();
    let labels: Vec<_> = forward
        .iter()
        .map(|group| (group.context.clone(), group.n))
        .collect();
    assert_eq!(
        labels,
        [(Some(context("first")), 10), (Some(context("second")), 10)]
    );
    records.reverse();
    let backward = Checker::new(&two, &records).unwrap().groups(&two).unwrap();
    assert_eq!(forward, backward);

    let mut halves = under("first", 1..=5);
    halves.extend(under("second", 6..=10));
    let checker = Checker::new(&two, &halves).unwrap();
    assert!(checker.groups(&two).unwrap().is_empty());
}

#[test]
fn an_unreadable_extract_is_refused_with_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let mut machine = machine(0.0);
    machine["facts"] = json!("missing.jsonl.gz");
    let path = dir.path().join("machine.json");
    let result = check(&path, &definition(&machine), &mut Extracts::new());
    let Err(error @ Error::ReadExtract(ReadError::Open { .. })) = result else {
        panic!("{result:?}");
    };
    let Error::ReadExtract(ReadError::Open { source, .. }) = &error else {
        unreachable!();
    };
    assert_eq!(source.kind(), io::ErrorKind::NotFound);
    let message = error.to_string();
    assert!(
        message.starts_with("read the machine's facts extract: "),
        "{message}"
    );
    assert!(message.contains("missing.jsonl.gz"), "{message}");
    assert!(!message.contains("replay oracle"), "{message}");
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn a_machine_without_an_extract_is_refused() {
    let result = check(
        Path::new("machine.json"),
        &definition(&json!({"cores": 2})),
        &mut Extracts::new(),
    );
    assert!(
        matches!(result, Err(Error::Extract(ExtractsError::NoFacts))),
        "{result:?}"
    );
}

#[test]
fn an_invalid_machine_is_refused_before_its_extract_is_read() {
    let result = check(
        Path::new("machine.json"),
        &definition(&json!({"cores": 3, "facts": "missing.jsonl.gz"})),
        &mut Extracts::new(),
    );
    let Err(error @ Error::Machine(_)) = result else {
        panic!("{result:?}");
    };
    assert!(
        error.to_string().contains("must be even, from 2 to 16"),
        "{error}"
    );
}

#[test]
fn a_trial_that_does_not_fit_the_machine_is_refused_by_its_place_in_the_extract() {
    let two = model(&json!({"cores": 2}));
    for (name, change, want) in [
        (
            "profile size",
            (|f: &mut TrialFact| f.profile = Profile::new(&[TargetOffset::ZERO]).unwrap())
                as fn(&mut TrialFact),
            FactProblem::ProfileSize { found: 1, cores: 2 },
        ),
        (
            "empty loaded",
            |f| {
                f.class = Class::Trial(TrialClass {
                    loaded: CoreSet::default(),
                    ..trial_class(f)
                });
            },
            FactProblem::NoLoadedCore,
        ),
        (
            "zero duration",
            |f| {
                f.class = Class::Trial(TrialClass {
                    duration_s: 0,
                    ..trial_class(f)
                });
            },
            FactProblem::ZeroDuration,
        ),
        (
            "core out of range",
            |f| {
                f.class = Class::Trial(TrialClass {
                    loaded: cores(&[2]),
                    ..trial_class(f)
                });
            },
            FactProblem::LoadedCore { core: 2, cores: 2 },
        ),
    ] {
        let mut record = fact(7, &[0, 0], &[0], 60, Decisive::Pass);
        change(&mut record);
        let result = Checker::new(&two, &[record]);
        let Err(error @ Error::InvalidFact { seq, problem, .. }) = result else {
            panic!("{name}: refused nothing");
        };
        assert_eq!((seq, problem), (Seq::new(7).unwrap(), want), "{name}");
        assert!(
            error
                .to_string()
                .starts_with(&format!("invalid trial fact {SESSION}:7: ")),
            "{name}: {error}"
        );
    }
}

fn trial_class(fact: &TrialFact) -> TrialClass {
    match fact.class {
        Class::Trial(class) => class,
        Class::Idle { .. } => unreachable!(),
    }
}

#[test]
fn a_trial_the_extract_holds_for_another_context_is_not_validated() {
    let declared = {
        let mut machine = machine(0.0);
        machine["bios_context"] = json!({"board": "mine"});
        model(&machine)
    };
    let mut foreign = fact(1, &[0], &[0], 60, Decisive::Pass);
    foreign.context = Some(context("other"));
    let checker = Checker::new(&declared, &[foreign]).unwrap();
    assert!(checker.groups(&declared).unwrap().is_empty());
}

#[test]
fn idle_failures_are_counted_without_inventing_exposure() {
    let sixteen = model(&json!({"cores": 16, "model": {"near_limit_rate": 0.0}}));
    let mut records: Vec<_> = (1..=10)
        .map(|seq| fact(seq, &[0; 16], &[0], 60, Decisive::Pass))
        .collect();
    records.extend([idle(11), idle(12)]);
    let result = check_records(
        Path::new("machine"),
        Path::new("extract"),
        &sixteen,
        &records,
    )
    .unwrap();
    let TrialClass { workload, .. } = trial_class(&records[0]);
    assert_eq!(result.machine, Path::new("machine"));
    assert_eq!(result.extract, Path::new("extract"));
    assert_eq!(result.status, Status::Ok);
    assert_eq!(result.idle_failures, 2);
    let [group] = result.groups.as_slice() else {
        panic!("{result:?}");
    };
    assert_eq!(
        group.class,
        TrialClass {
            workload,
            loaded: cores(&[0]),
            duration_s: 60
        }
    );
    assert_eq!((group.n, group.k, group.mean_p), (10, 0, 0.0));
    assert_eq!(group.interval, Interval { low: 0, high: 0 });
    assert!(!group.flagged);
    assert_eq!(Checker::new(&sixteen, &records).unwrap().idle_failures(), 2);
}

#[test]
fn flagged_lists_only_the_groups_outside_their_interval() {
    let two = model(&machine(0.0));
    let mut records = passes(10);
    records.extend((11..=20).map(|seq| fact(seq, &[-20, 0], &[0], 90, crash())));
    let result = check_records(Path::new("m"), Path::new("e"), &two, &records).unwrap();
    assert_eq!(result.status, Status::Flagged);
    let flagged: Vec<_> = result
        .flagged()
        .map(|g| (g.depth.get(), g.n, g.k))
        .collect();
    assert_eq!(flagged, [(-20, 10, 10)]);
    assert_eq!(result.groups.len(), 2);
}

#[test]
fn a_predictor_of_another_shape_is_an_error_not_a_verdict() {
    let two = model(&machine(0.0));
    let four = model(&json!({"cores": 4}));
    let checker = Checker::new(&two, &passes(10)).unwrap();
    assert!(checker.accepts(&four).is_err());
    assert!(checker.groups(&four).is_err());
    assert!(checker.check("m", "e", &four).is_err());
}

#[test]
fn a_display_of_a_group_names_its_class_depth_and_counts() {
    let two = model(&machine(0.0));
    let mut records: Vec<_> = (1..=10)
        .map(|seq| fact(seq, &[-24, -24], &[0, 1], 120, Decisive::Pass))
        .collect();
    records[0].outcome = crash();
    let groups = Checker::new(&two, &records).unwrap().groups(&two).unwrap();
    assert_eq!(
        groups[0].to_string(),
        format!(
            "R1 {} cores=0,1 duration=120s depth=-24 n=10 k=1",
            workload()
        )
    );
}

struct Budget(usize);

impl io::Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0 < bytes.len() {
            return Err(io::ErrorKind::WriteZero.into());
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_sink_that_fails_anywhere_in_the_report_fails_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let flagged = check_file(&write_fixture(dir.path(), &machine(1.0))).unwrap();
    let mut whole = Vec::new();
    report(&mut whole, std::slice::from_ref(&flagged)).unwrap();
    for budget in 0..whole.len() {
        assert!(
            report(&mut Budget(budget), std::slice::from_ref(&flagged)).is_err(),
            "budget {budget} of {}",
            whole.len()
        );
    }
    assert!(report(&mut Budget(whole.len()), &[flagged]).is_ok());
}

#[test]
fn a_formatter_that_fails_fails_the_display_of_a_group() {
    struct Fails;
    impl std::fmt::Write for Fails {
        fn write_str(&mut self, _: &str) -> std::fmt::Result {
            Err(std::fmt::Error)
        }
    }
    let two = model(&machine(0.0));
    let group = Checker::new(&two, &passes(10))
        .unwrap()
        .groups(&two)
        .unwrap()
        .remove(0);
    assert!(std::fmt::write(&mut Fails, format_args!("{group}")).is_err());
}
