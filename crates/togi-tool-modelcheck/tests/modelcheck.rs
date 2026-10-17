#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde_json::{Value, json};
use test_case::test_case;
use togi_facts::{Decisive, TrialClass};
use togi_journal::{FixStamp, SCHEMA, Seq};
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
    BuildStamp, Class, Extracts, ExtractsError, Phase, Profile, ReadError, TrialFact, decode,
    encode,
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
            fixes: FixStamp::none(),
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

fn check_fixture(machine: &Value) -> Result<Check, Error> {
    let mut bytes = Vec::new();
    encode(&mut bytes, &passes(10)).unwrap();
    let path = Path::new("fixture/machine.json");
    let definition = definition(machine);
    let mut extracts = Extracts::new();
    extracts.load_with(path, &definition, |extract| {
        assert_eq!(extract, Path::new("fixture/facts.jsonl.gz"));
        decode(bytes.as_slice())
    })?;
    check(path, &definition, &mut extracts)
}

#[test_case(0.0, Status::Ok, Interval { low: 0, high: 0 }; "never near its limit")]
#[test_case(1.0, Status::Flagged, Interval { low: 10, high: 10 }; "always near its limit")]
fn a_fixture_machine_is_judged_against_its_own_extract(
    near_limit_rate: f64,
    status: Status,
    interval: Interval,
) {
    let path = Path::new("fixture/machine.json");
    let result = check_fixture(&machine(near_limit_rate)).unwrap();
    assert_eq!(result.status, status);
    assert_eq!(result.machine, path);
    assert_eq!(result.extract, Path::new("fixture/facts.jsonl.gz"));
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

#[test]
fn the_report_names_each_flagged_group_and_nothing_for_no_checks() {
    let mut out = Vec::new();
    report(&mut out, &[]).unwrap();
    assert!(out.is_empty());

    let path = Path::new("fixture/machine.json");
    let flagged = check_fixture(&machine(1.0)).unwrap();
    let ok = check_fixture(&machine(0.0)).unwrap();
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
    let result = check_fixture(&machine(0.0)).unwrap();
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

#[test_case(&[1], [0, -30, 0, -45], -30; "alone")]
#[test_case(&[0, 1, 2], [-40, -20, -30, 0], -20; "together")]
#[test_case(&[0, 1, 2], [-40, 0, -30, 0], 0; "any-zero")]
fn a_group_is_keyed_by_the_shallowest_loaded_offset_and_idle_cores_do_not_count(
    loaded: &[i64],
    profile: [i64; 4],
    depth: i64,
) {
    let four = model(&json!({"cores": 4}));
    let records: Vec<_> = (1..=10)
        .map(|seq| fact(seq, &profile, loaded, 60, Decisive::Pass))
        .collect();
    let groups = Checker::new(&four, &records)
        .unwrap()
        .groups(&four)
        .unwrap();
    let [group] = groups.as_slice() else {
        panic!("{groups:?}");
    };
    assert_eq!(group.depth.get(), depth);
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

fn flat(p: f64) -> Value {
    json!({"cores": 2, "core": limits(-(-p).ln_1p() / 90.0)})
}

#[test_case(0.0, false; "never")]
#[test_case(0.001, false; "too rarely")]
#[test_case(0.1, true; "as recorded")]
#[test_case(0.9, false; "too often")]
fn a_checker_accepts_exactly_what_its_report_calls_ok(p: f64, want: bool) {
    let mut records: Vec<_> = passes(20);
    for record in &mut records[..2] {
        record.outcome = crash();
    }
    let checker = Checker::new(&model(&flat(0.1)), &records).unwrap();
    let model = model(&flat(p));
    let result = checker.check("synthetic", "synthetic", &model).unwrap();
    assert_eq!(checker.accepts(&model).unwrap(), want);
    assert_eq!(result.status == Status::Ok, want, "{result:?}");
}

fn with_reset(kind: ResetKind, weight: f64) -> Result<Check, Error> {
    let mut machine = machine(0.0);
    machine["model"]["reset"] = json!({"watchdog": 1.0, kind.as_str(): weight});
    check_fixture(&machine)
}

#[test_case(ResetKind::ThermalTrip, 1e-9; "thermal trip small-positive")]
#[test_case(ResetKind::ThermalTrip, 1.0; "thermal trip positive")]
#[test_case(ResetKind::PowerLoss, 1e-9; "power loss small-positive")]
#[test_case(ResetKind::PowerLoss, 1.0; "power loss positive")]
fn a_machine_with_an_indecisive_reset_is_refused(kind: ResetKind, weight: f64) {
    let result = with_reset(kind, weight);
    let Err(Error::IndecisiveReset(refused)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(refused, kind);
}

#[test_case(ResetKind::ThermalTrip; "thermal trip")]
#[test_case(ResetKind::PowerLoss; "power loss")]
fn a_zero_weight_indecisive_reset_changes_nothing(kind: ResetKind) {
    let result = with_reset(kind, 0.0).unwrap();
    assert_eq!(result.status, Status::Ok);
    assert_eq!(
        result.groups.iter().map(|g| (g.n, g.k)).collect::<Vec<_>>(),
        [(10, 0)]
    );
}

fn declared(board: &str) -> Model {
    let mut machine = machine(0.0);
    machine["bios_context"] = json!({
        "bios_version": "v1", "board": board, "cpu_model": "cpu", "microcode": "0x1", "boost_limit_mhz": 5000,
    });
    model(&machine)
}

#[test_case(9, Some("fixture"), None, 0; "below-ten")]
#[test_case(10, Some("fixture"), None, 1; "ten")]
#[test_case(10, Some("other"), None, 1; "undeclared-takes-any")]
#[test_case(10, None, None, 1; "undeclared-takes-none")]
#[test_case(10, None, Some("fixture"), 0; "declared-without-context")]
#[test_case(10, Some("fixture"), Some("other"), 0; "mismatch")]
#[test_case(10, Some("fixture"), Some("fixture"), 1; "match")]
fn the_machines_declared_context_limits_which_facts_are_checked(
    count: u64,
    recorded: Option<&str>,
    machine_board: Option<&str>,
    groups: usize,
) {
    let mut records = passes(count);
    for record in &mut records {
        record.context = recorded.map(context);
    }
    let model = machine_board.map_or_else(|| model(&machine(0.0)), declared);
    let checker = Checker::new(&model, &records).unwrap();
    let groups_found = checker.groups(&model).unwrap();
    assert_eq!(groups_found.len(), groups);
    if groups == 1 {
        assert_eq!(
            (groups_found[0].n, groups_found[0].k),
            (count.try_into().unwrap(), 0)
        );
    }
}

#[test]
fn a_group_needs_ten_trials() {
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
fn contexts_order_by_their_boost_limit_as_text() {
    let two = model(&machine(0.0));
    let boosted = |boost_limit_mhz: u32| BiosContext {
        boost_limit_mhz,
        ..context("board")
    };
    let mut records = Vec::new();
    for (boost, seqs) in [(5700, 1..=10), (570, 11..=20), (10_000, 21..=30)] {
        records.extend(seqs.map(|seq| {
            let mut record = fact(seq, &[-10, 0], &[0], 90, Decisive::Pass);
            record.context = Some(boosted(boost));
            record
        }));
    }
    let groups = Checker::new(&two, &records).unwrap().groups(&two).unwrap();
    let boosts: Vec<_> = groups
        .iter()
        .map(|group| group.context.as_ref().unwrap().boost_limit_mhz)
        .collect();
    assert_eq!(boosts, [10_000, 570, 5700]);
}

#[test]
fn a_failed_extract_read_is_reported_with_its_path() {
    let mut machine = machine(0.0);
    machine["facts"] = json!("missing.jsonl.gz");
    let result = Extracts::new()
        .load_with(
            Path::new("fixture/machine.json"),
            &definition(&machine),
            |path| {
                Err(ReadError::Open {
                    path: path.to_path_buf(),
                    source: io::ErrorKind::NotFound.into(),
                })
            },
        )
        .map_err(Error::from);
    let Err(error @ Error::ReadExtract(ReadError::Open { .. })) = result else {
        panic!("{result:?}");
    };
    let Error::ReadExtract(ReadError::Open { source, .. }) = &error else {
        unreachable!();
    };
    assert_eq!(source.kind(), io::ErrorKind::NotFound);
    let message = togi_machine::error_chain(&error);
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
    let message = togi_machine::error_chain(&error);
    assert!(message.contains("must be even, from 2 to 16"), "{message}");
}

fn one_offset(f: &mut TrialFact) {
    f.profile = Profile::new(&[TargetOffset::ZERO]).unwrap();
}

fn nothing_loaded(f: &mut TrialFact) {
    f.class = Class::Trial(TrialClass {
        loaded: CoreSet::default(),
        ..trial_class(f)
    });
}

fn no_duration(f: &mut TrialFact) {
    f.class = Class::Trial(TrialClass {
        duration_s: 0,
        ..trial_class(f)
    });
}

fn a_third_core_loaded(f: &mut TrialFact) {
    f.class = Class::Trial(TrialClass {
        loaded: cores(&[2]),
        ..trial_class(f)
    });
}

#[test_case(one_offset, FactProblem::ProfileSize { found: 1, cores: 2 }; "profile size")]
#[test_case(nothing_loaded, FactProblem::NoLoadedCore; "empty loaded")]
#[test_case(no_duration, FactProblem::ZeroDuration; "zero duration")]
#[test_case(a_third_core_loaded, FactProblem::LoadedCore { core: 2, cores: 2 }; "core out of range")]
fn a_trial_that_does_not_fit_the_machine_is_refused_by_its_place_in_the_extract(
    change: fn(&mut TrialFact),
    want: FactProblem,
) {
    let two = model(&json!({"cores": 2}));
    let mut record = fact(7, &[0, 0], &[0], 60, Decisive::Pass);
    change(&mut record);
    let result = Checker::new(&two, &[record]);
    let Err(error @ Error::InvalidFact { seq, problem, .. }) = result else {
        panic!("refused nothing");
    };
    assert_eq!((seq, problem), (Seq::new(7).unwrap(), want));
    assert!(
        error
            .to_string()
            .starts_with(&format!("invalid trial fact {SESSION}:7: ")),
        "{error}"
    );
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
    let flagged = check_fixture(&machine(1.0)).unwrap();
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
