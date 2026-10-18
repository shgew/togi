use crate::common::Tree;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use togi_tool_bench::{Error, Selection, Suite};
use togi_tool_forecast::Split;

fn bench_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench")
}

fn plan_in(dir: &Tree, suite: &str, selection: Selection) -> Result<Vec<String>, Error> {
    let suite = Suite::parse(suite.as_bytes(), dir.path().to_path_buf())?;
    Ok(suite
        .plan_from(selection, dir)?
        .runs
        .iter()
        .map(togi_tool_bench::RunSpec::name)
        .collect())
}

fn plan(suite: &str, selection: Selection) -> Result<Vec<String>, Error> {
    let dir = Tree::new();
    dir.write(dir.path().join("a.json"), "{\"cores\": 2}");
    dir.write(dir.path().join("b.json"), "{\"cores\": 2}");
    plan_in(&dir, suite, selection)
}

#[test_case::test_case(("syntax", "[", "decode suite"); "syntax")]
#[test_case::test_case(("unknown key", r#"{"extra":true}"#, "unknown field"); "unknown key")]
#[test_case::test_case((
            "unknown scenario key",
            r#"{"scenarios":[{"name":"x","dev":[1],"extra":1}]}"#,
            "unknown field",
        ); "unknown scenario key")]
#[test_case::test_case((
            "positional suite",
            r#"[null,[{"name":"x","dev":[1]}]]"#,
            "decode suite",
        ); "positional suite")]
#[test_case::test_case((
            "positional scenario",
            r#"{"scenarios":[["x"]]}"#,
            "decode suite",
        ); "positional scenario")]
#[test_case::test_case((
            "empty name",
            r#"{"scenarios":[{"name":"","dev":[1]}]}"#,
            "invalid scenario name",
        ); "empty name")]
#[test_case::test_case((
            "duplicate name",
            r#"{"scenarios":[{"name":"x","dev":[1]},{"name":"x","dev":[2]}]}"#,
            "duplicate scenario",
        ); "duplicate name")]
#[test_case::test_case((
            "nested name",
            r#"{"scenarios":[{"name":"x/y","dev":[1]}]}"#,
            "invalid scenario name",
        ); "nested name")]
#[test_case::test_case((
            "dot name",
            r#"{"scenarios":[{"name":".","dev":[1]}]}"#,
            "invalid scenario name",
        ); "dot name")]
#[test_case::test_case((
            "parent name",
            r#"{"scenarios":[{"name":"..","dev":[1]}]}"#,
            "invalid scenario name",
        ); "parent name")]
#[test_case::test_case((
            "conflicting machines",
            r#"{"scenarios":[{"name":"x","machine":"a.json","machines":["b.json"],"dev":[1]}]}"#,
            "mutually exclusive",
        ); "conflicting machines")]
#[test_case::test_case((
            "an empty ensemble member",
            r#"{"scenarios":[{"name":"x","machines":[""],"dev":[1]}]}"#,
            "empty path",
        ); "an empty ensemble member")]
#[test_case::test_case((
            "missing machine",
            r#"{"scenarios":[{"name":"x","machine":"missing.json","dev":[1]}]}"#,
            "load scenario x",
        ); "missing machine")]
#[test_case::test_case((
            "replay without facts",
            r#"{"scenarios":[{"name":"x","machine":"a.json","replay":true,"dev":[1]}]}"#,
            "load scenario x",
        ); "replay without facts")]
#[test_case::test_case((
            "replay without a machine",
            r#"{"scenarios":[{"name":"x","replay":true,"dev":[1]}]}"#,
            "replay needs a machine",
        ); "replay without a machine")]
#[test_case::test_case((
            "repeated selected seed",
            r#"{"scenarios":[{"name":"x","dev":[1,1]}]}"#,
            "scenario x repeats seed 1",
        ); "repeated selected seed")]
#[test_case::test_case((
            "repeated unselected seed",
            r#"{"scenarios":[{"name":"x","dev":[1],"holdout":[2,2]}]}"#,
            "scenario x repeats seed 2",
        ); "repeated unselected seed")]
#[test_case::test_case((
            "overlapping splits",
            r#"{"scenarios":[{"name":"x","dev":[1],"holdout":[1]}]}"#,
            "scenario x repeats seed 1",
        ); "overlapping splits")]
#[test_case::test_case((
            "smoke seed outside both splits",
            r#"{"scenarios":[{"name":"x","dev":[1],"smoke":[2]}]}"#,
            "smoke seed 2 is in neither dev nor holdout",
        ); "smoke seed outside both splits")]
#[test_case::test_case((
            "no selected runs",
            r#"{"scenarios":[{"name":"x","holdout":[1]}]}"#,
            "suite has no selected runs",
        ); "no selected runs")]
fn an_invalid_suite_is_refused_with_the_reason(case: (&str, &str, &str)) {
    let (name, suite, want) = case;

    let error = togi_machine::error_chain(&plan(suite, Selection::Dev).unwrap_err());
    assert!(error.contains(want), "{name}: {error}");
}

#[test_case::test_case((
            "a holdout-only missing machine",
            r#"{"scenarios":[{"name":"x","dev":[1]},{"name":"y","machine":"missing.json","holdout":[1]}]}"#,
            "load scenario y",
        ); "a holdout-only missing machine")]
#[test_case::test_case((
            "an ensemble member only holdout positions reach",
            r#"{"scenarios":[{"name":"x","machines":["a.json","missing.json"],"dev":[1],"holdout":[2]}]}"#,
            "load scenario x",
        ); "an ensemble member only holdout positions reach")]
#[test_case::test_case((
            "a holdout-only replay without facts",
            r#"{"scenarios":[{"name":"x","dev":[1]},{"name":"y","machine":"a.json","replay":true,"holdout":[1]}]}"#,
            "load scenario y",
        ); "a holdout-only replay without facts")]
#[test_case::test_case((
            "a holdout-only replay without a machine",
            r#"{"scenarios":[{"name":"x","dev":[1]},{"name":"y","replay":true,"holdout":[1]}]}"#,
            "replay needs a machine",
        ); "a holdout-only replay without a machine")]
fn every_scenario_is_checked_whatever_the_selection(case: (&str, &str, &str)) {
    let (name, suite, want) = case;

    let error = togi_machine::error_chain(&plan(suite, Selection::Dev).unwrap_err());
    assert!(error.contains(want), "{name}: {error}");

    let parse = Suite::parse(
        br#"{"scenarios":[{"name":"y","replay":true,"dev":[1]}]}"#,
        PathBuf::new(),
    );
    assert!(matches!(
        parse,
        Err(togi_tool_bench::SuiteError::ReplayWithoutMachine(_))
    ));
}

#[test]
fn a_missing_suite_file_is_a_read_error() {
    let dir = Tree::new();
    let error = Suite::load_from(&dir.path().join("missing.json"), &dir)
        .unwrap_err()
        .to_string();
    assert!(error.contains("read suite"), "{error}");
}

#[test]
fn runs_come_in_suite_order_with_dev_seeds_before_holdout_seeds() {
    let names = plan(
        r#"{"scenarios":[{"name":"y","dev":[2,1],"holdout":[9]},{"name":"x","dev":[1]}]}"#,
        Selection::All,
    )
    .unwrap();
    assert_eq!(names, ["y/dev-2", "y/dev-1", "y/holdout-9", "x/dev-1"]);
    let holdout = plan(
        r#"{"scenarios":[{"name":"y","dev":[2],"holdout":[9]}]}"#,
        Selection::Holdout,
    )
    .unwrap();
    assert_eq!(holdout, ["y/holdout-9"]);
}

#[test]
fn smoke_seeds_mark_their_runs() {
    let dir = Tree::new();
    let suite = Suite::parse(
        br#"{"scenarios":[{"name":"x","dev":[1,2],"holdout":[3],"smoke":[2,3]},{"name":"y","dev":[1]}]}"#,
        dir.path().to_path_buf(),
    )
    .unwrap();
    let smoke: Vec<_> = suite
        .plan_from(Selection::All, &dir)
        .unwrap()
        .runs
        .iter()
        .filter(|run| run.smoke)
        .map(togi_tool_bench::RunSpec::name)
        .collect();
    assert_eq!(smoke, ["x/dev-2", "x/holdout-3"]);
}

#[test]
fn an_ensemble_picks_members_by_position_within_each_split() {
    let dir = Tree::new();
    dir.write(dir.path().join("a.json"), "{\"cores\": 2}");
    dir.write(dir.path().join("b.json"), "{\"cores\": 2}");
    let suite = Suite::parse(
        br#"{"scenarios":[{"name":"x","machines":["a.json","b.json"],"dev":[10,20,30],"holdout":[99,98]}]}"#,
        dir.path().to_path_buf(),
    )
    .unwrap();
    let plan = suite.plan_from(Selection::All, &dir).unwrap();
    let members: Vec<_> = plan
        .runs
        .iter()
        .map(|run| (run.name(), run.machine.as_ref().unwrap().name.clone()))
        .collect();
    assert_eq!(
        members,
        [
            ("x/dev-10", "a.json"),
            ("x/dev-20", "b.json"),
            ("x/dev-30", "a.json"),
            ("x/holdout-99", "a.json"),
            ("x/holdout-98", "b.json"),
        ]
        .map(|(run, machine)| (run.to_owned(), machine.to_owned()))
    );
    assert_eq!(plan.machines.len(), 2);
}

#[test]
fn a_machine_shared_by_runs_is_read_once() {
    let dir = Tree::new();
    dir.write(dir.path().join("a.json"), "{\"cores\": 2}");
    let suite = Suite::parse(
        br#"{"scenarios":[{"name":"x","machine":"a.json","dev":[1,2]},{"name":"y","machine":"a.json","dev":[1]}]}"#,
        dir.path().to_path_buf(),
    )
    .unwrap();
    let plan = suite.plan_from(Selection::Dev, &dir).unwrap();
    assert_eq!(plan.machines.len(), 1);
    let first = plan.runs[0].machine.as_ref().unwrap();
    assert!(
        plan.runs
            .iter()
            .all(|run| std::sync::Arc::ptr_eq(run.machine.as_ref().unwrap(), first))
    );
}

#[test_case::test_case(r#"{"scenarios":[{"name":"x","dev":[1]}]}"#; "r scenarios name x dev 1")]
#[test_case::test_case(r#"{"gate":{"id":"g"},"scenarios":[{"name":"x","dev":[1]}]}"#; "r gate id g scenarios name x dev 1")]
fn the_registered_gates_notes_are_the_suites_notes(case: &str) {
    let text = case;
    let dir = Tree::new();
    let with = Suite::parse(
        br#"{"gate":{"id":"g","notes":["first","second"]},"scenarios":[{"name":"x","dev":[1]}]}"#,
        dir.path().to_path_buf(),
    )
    .unwrap();
    assert_eq!(with.notes(), ["first", "second"]);

    let without = Suite::parse(text.as_bytes(), dir.path().to_path_buf()).unwrap();
    assert!(without.notes().is_empty());
}

#[test]
fn the_digest_follows_the_suite_files_bytes() {
    let dir = Tree::new();
    let digest = |text: &str| {
        Suite::parse(text.as_bytes(), dir.path().to_path_buf())
            .unwrap()
            .digest()
            .to_owned()
    };
    let one = r#"{"scenarios":[{"name":"x","dev":[1]}]}"#;
    assert_eq!(digest(one), digest(one));
    assert_ne!(digest(one), digest(&one.replace('1', "2")));
}

#[test]
fn the_committed_suite_has_the_runs_and_scenarios_it_documents() {
    let dir = Tree::committed();
    let suite = dir.suite();
    let all = suite.plan_from(Selection::All, &dir).unwrap();
    let dev = suite.plan_from(Selection::Dev, &dir).unwrap();
    assert_eq!((all.runs.len(), dev.runs.len()), (220, 110));
    let mut names: Vec<&str> = Vec::new();
    for run in &all.runs {
        if !names.contains(&run.scenario.as_str()) {
            names.push(&run.scenario);
        }
    }
    assert_eq!(
        names,
        [
            "default",
            "target",
            "flat-hazard",
            "late-onset",
            "idle-limit",
            "misleading-mce",
            "target-nonmember-mce",
            "shared-voltage",
            "target-shared-voltage",
            "target-r7-vf-boost",
            "target-r7-request-gap",
        ]
    );
    assert!(suite.notes().is_empty());
}

#[test]
fn the_target_scenario_spreads_two_seeds_per_member_over_nine_replayed_fits() {
    let dir = Tree::committed();
    let plan = dir.suite().plan_from(Selection::All, &dir).unwrap();
    let mut members: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for run in plan.runs.iter().filter(|run| run.scenario == "target") {
        assert!(run.replay.is_some());
        let counts = members
            .entry(run.machine.as_ref().unwrap().name.clone())
            .or_default();
        match run.split {
            Split::Dev => counts.0 += 1,
            Split::Holdout => counts.1 += 1,
        }
    }
    let want: BTreeMap<String, (usize, usize)> = (0..9)
        .map(|i| (format!("machines/target-fit-{i}.json"), (2, 2)))
        .collect();
    assert_eq!(members, want);
}

#[test]
fn the_committed_nonmember_scenario_is_a_replayed_synthetic_guard_with_one_smoke_seed() {
    let dir = Tree::committed();
    let plan = dir.suite().plan_from(Selection::All, &dir).unwrap();
    let runs: Vec<_> = plan
        .runs
        .iter()
        .filter(|run| run.scenario == "target-nonmember-mce")
        .collect();
    assert!(runs.iter().all(|run| run.replay.is_some()));
    assert!(
        runs.iter()
            .all(|run| run.machine.as_ref().unwrap().name == "machines/target-nonmember-mce.json")
    );
    let seeds = |split: Split, smoke: bool| -> Vec<u64> {
        runs.iter()
            .filter(|run| run.split == split && (!smoke || run.smoke))
            .map(|run| run.seed)
            .collect()
    };
    assert_eq!(seeds(Split::Dev, false), [1, 2, 3, 4]);
    assert_eq!(seeds(Split::Holdout, false), [101, 102, 103, 104]);
    assert_eq!(
        runs.iter()
            .filter(|run| run.smoke)
            .map(|run| run.seed)
            .collect::<Vec<_>>(),
        [103]
    );

    let machine = &runs[0].machine.as_ref().unwrap().definition;
    for want in ["Synthetic", "#530", "not an admitted adversary"] {
        assert!(
            machine.description.contains(want),
            "{}",
            machine.description
        );
    }
    assert!(
        !machine.description.contains("all parameters except")
            && !machine.description.contains("unchanged"),
        "{}",
        machine.description
    );
    assert_eq!(
        [machine.model.past_limit_rate, machine.model.growth],
        [0.002_906_799_572_937_778_2, 2.575_493_297_181_685_3],
        "the frozen numeric model"
    );
    assert_eq!(machine.cores, 16);
    assert_eq!(machine.core.len(), 16);
    assert!(!machine.joint.is_empty());
}

#[test]
fn every_machine_of_the_committed_suite_is_valid_and_lists_its_files() {
    let dir = Tree::committed();
    let plan = dir.suite().plan_from(Selection::All, &dir).unwrap();
    assert_eq!(plan.machines.len(), 18);
    for machine in &plan.machines {
        assert!(machine.model(1).is_ok(), "{}", machine.name);
        assert!(
            machine.inputs.iter().all(|(_, sum)| sum.is_some()),
            "{}",
            machine.name
        );
    }
    let overlay = plan
        .machines
        .iter()
        .find(|m| m.name.ends_with("target-r7-vf-boost.json"))
        .unwrap();
    let files: Vec<_> = overlay
        .inputs
        .iter()
        .map(|(path, _)| path.to_string_lossy().into_owned())
        .collect();
    assert_eq!(files[0], "machines/target-r7-vf-boost.json");
    assert_eq!(files[1], "machines/target-shared-voltage.json");
}

/// The #490 research suite in a memory tree: never part of the committed bench suite.
fn research_490() -> (Tree, Suite) {
    let dir = Tree::at("/research/490");
    macro_rules! machine {
        ($($name:literal),+ $(,)?) => {
            $(dir.write(
                dir.path().join(concat!("machines/", $name, ".json")),
                include_bytes!(concat!("../../../../tools/bench/research/490/machines/", $name, ".json")),
            );)+
        };
    }
    machine!(
        "zero",
        "background-0.1",
        "background-0.5",
        "background-1",
        "background-2",
        "background-4",
        "background-8",
        "background-16",
        "depth-0.1",
        "depth-0.5",
        "depth-1",
        "depth-2",
        "depth-4",
        "depth-8",
        "depth-16",
    );
    let suite = Suite::parse(
        include_bytes!("../../../../tools/bench/research/490/suite.json"),
        dir.path().to_path_buf(),
    )
    .unwrap();
    (dir, suite)
}

#[test]
fn the_490_research_suite_runs_24_dev_seeds_of_each_hazard_level_and_form() {
    let (dir, suite) = research_490();
    let plan = suite.plan_from(Selection::All, &dir).unwrap();
    assert_eq!(plan.machines.len(), 15);
    assert_eq!(plan.runs.len(), 15 * 24);
    assert!(plan.runs.iter().all(|run| run.split == Split::Dev));
    let seeds: Vec<u64> = plan
        .runs
        .iter()
        .filter(|run| run.scenario == "zero")
        .map(|run| run.seed)
        .collect();
    assert_eq!(seeds, (1..=24).collect::<Vec<_>>());
}

#[test]
fn the_490_research_hazard_is_its_level_per_loaded_ccd_in_its_form() {
    use std::time::Duration;
    use togi_machine::{CoreId, CoreSet, Regime, TargetOffset};
    use togi_sim::{Load, SourceKind};

    let (dir, suite) = research_490();
    let plan = suite.plan_from(Selection::Dev, &dir).unwrap();
    let cores = |range: std::ops::Range<i64>| -> CoreSet {
        range.map(|core| CoreId::new(core).unwrap()).collect()
    };
    let load = |cores: CoreSet| Load {
        regime: Regime::R7,
        workload: "SSE",
        cores,
        duration: Duration::from_secs(600),
    };
    let ccd_rate = |run: &togi_tool_bench::RunSpec, depth: i64, load: &Load<'_>| -> f64 {
        let profile = vec![TargetOffset::new(-depth).unwrap(); 16];
        run.model()
            .unwrap()
            .sources(&profile, load)
            .unwrap()
            .iter()
            .filter(|source| matches!(source.kind, SourceKind::Ccd(_)))
            .map(|source| source.rate * 3600.0)
            .sum()
    };
    let close = |got: f64, want: f64, what: &str| {
        assert!(
            (got - want).abs() <= want.max(1.0) * 1e-9,
            "{what}: {got} per hour, want {want}"
        );
    };
    let pair = load(cores(0..2));
    let all = load(cores(0..16));
    for run in plan.runs.iter().filter(|run| run.seed == 1) {
        let name = run.scenario.as_str();
        if name == "zero" {
            close(ccd_rate(run, 25, &all), 0.0, name);
            continue;
        }
        let (form, level) = name.split_once('-').unwrap();
        let h: f64 = level.parse().unwrap();
        close(ccd_rate(run, 25, &pair), h, name);
        close(ccd_rate(run, 25, &all), 2.0 * h, name);
        match form {
            "background" => {
                close(ccd_rate(run, 0, &pair), h, name);
                close(ccd_rate(run, 40, &pair), h, name);
            }
            "depth" => {
                close(ccd_rate(run, 30, &pair), 2.0 * h, name);
                close(ccd_rate(run, 20, &pair), h / 2.0, name);
            }
            _ => panic!("{name}: not a hazard form"),
        }
    }
}

fn leaves(prefix: &str, value: &serde_json::Value, out: &mut BTreeMap<String, serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, inner) in map {
                leaves(&format!("{prefix}/{key}"), inner, out);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, inner) in items.iter().enumerate() {
                leaves(&format!("{prefix}/{index}"), inner, out);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {
            out.insert(prefix.to_owned(), value.clone());
        }
    }
}

fn leaves_of(name: &str) -> BTreeMap<String, serde_json::Value> {
    let dir = Tree::committed();
    let path = dir.path().join("machines").join(name);
    let (definition, _) = togi_sim::file::load_from(path, &dir).unwrap();
    let mut out = BTreeMap::new();
    leaves("", &serde_json::to_value(definition).unwrap(), &mut out);
    out
}

#[derive(Clone, Copy)]
enum OverlayMechanism {
    VfBoost,
    RequestGap,
}

#[test_case::test_case(OverlayMechanism::VfBoost; "VfBoost")]
#[test_case::test_case(OverlayMechanism::RequestGap; "RequestGap")]
fn the_anchor_overlays_differ_from_the_anchor_only_by_their_mechanism(scenario: OverlayMechanism) {
    const AVX2: &str = "/shared_voltage/workload/mprime-avx2-36k-248k-allcore/core";
    const AVX512: &str = "/shared_voltage/workload/mprime-avx512-36k-248k-allcore/core";
    let anchor = leaves_of("target-shared-voltage.json");

    let (name, edits) = match scenario {
        OverlayMechanism::VfBoost => (
            "target-r7-vf-boost.json",
            vec![(format!("{AVX2}/5/threshold_clock_v_per_100mhz"), 0.06)],
        ),
        OverlayMechanism::RequestGap => (
            "target-r7-request-gap.json",
            [(9, 1.18), (10, 1.22), (11, 1.18)]
                .into_iter()
                .flat_map(|(core, threshold)| {
                    [
                        (format!("{AVX512}/{core}/threshold_v"), threshold),
                        (
                            format!("{AVX512}/{core}/threshold_clock_v_per_100mhz"),
                            0.06,
                        ),
                    ]
                })
                .collect(),
        ),
    };

    let overlay = leaves_of(name);
    let mut expected = anchor.clone();
    for (path, value) in &edits {
        expected.insert(path.clone(), serde_json::json!(value));
    }
    let metadata = |path: &str| path == "/description" || path.starts_with("/notes/");
    expected.retain(|path, _| !metadata(path));
    let overlay_body: BTreeMap<_, _> = overlay
        .into_iter()
        .filter(|(path, _)| !metadata(path))
        .collect();
    assert_eq!(
        overlay_body, expected,
        "{name} is the anchor plus its mechanism"
    );
    assert_ne!(
        overlay_body,
        anchor
            .iter()
            .filter(|(p, _)| !metadata(p))
            .map(|(p, v)| (p.clone(), v.clone()))
            .collect(),
        "{name}"
    );
}

#[test]
fn the_896_research_suite_runs_unreplayed_dev_seeds_from_201_on_committed_machines() {
    let dir = Tree::committed();
    let suite = Suite::parse(
        include_bytes!("../../../../tools/bench/research/896/suite.json"),
        dir.path().join("research/896"),
    )
    .unwrap();
    let plan = suite.plan_from(Selection::All, &dir).unwrap();
    assert!(
        plan.runs
            .iter()
            .all(|run| run.split == Split::Dev && run.replay.is_none())
    );
    let mut got: Vec<(String, String, Vec<u64>)> = Vec::new();
    for run in &plan.runs {
        let machine = run.machine.as_ref().unwrap().name.clone();
        match got.last_mut() {
            Some((scenario, seen, seeds)) if *scenario == run.scenario && *seen == machine => {
                seeds.push(run.seed);
            }
            _ => got.push((run.scenario.clone(), machine, vec![run.seed])),
        }
    }
    let mut want: Vec<(String, String, Vec<u64>)> = (0..9)
        .map(|i| {
            let name = format!("target-fit-{i}");
            let machine = format!("../../machines/{name}.json");
            (name, machine, vec![201, 202])
        })
        .collect();
    for name in [
        "target-shared-voltage",
        "target-r7-request-gap",
        "target-r7-vf-boost",
        "shared-voltage",
    ] {
        let machine = format!("../../machines/{name}.json");
        want.push((name.to_owned(), machine, (201..=208).collect()));
    }
    assert_eq!(got, want);
    assert_eq!(plan.runs.len(), 50);

    let committed = dir.suite().plan_from(Selection::All, &dir).unwrap();
    assert!(
        committed.runs.iter().all(|run| run.seed < 201),
        "the committed suite reaches the research seeds"
    );
    for machine in &plan.machines {
        assert!(machine.model(201).is_ok(), "{}", machine.name);
        assert!(
            machine.inputs.iter().all(|(_, sum)| sum.is_some()),
            "{}",
            machine.name
        );
    }
}

#[test_case::test_case("target-r4-limit"; "target r4 limit")]
#[test_case::test_case("target-delayed-joint"; "target delayed joint")]
#[test_case::test_case("target-flat-risk"; "target flat risk")]
#[test_case::test_case("target-flat-cost"; "target flat cost")]
fn real_fs_deleted_scenario_files_are_absent_from_the_committed_tree(case: &str) {
    let deleted = case;

    let file = bench_dir().join("machines").join(format!("{deleted}.json"));
    assert!(!file.exists(), "{} is back", file.display());
}
