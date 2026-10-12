use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use togi_tool_sim::{Run, Split, Suite};

pub(crate) fn suite_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench/suite.json")
}

pub(crate) fn real_runs() -> Vec<Run> {
    Suite::load(&suite_path()).unwrap().runs()
}

fn count(runs: &[Run], scenario: &str, split: Split) -> usize {
    runs.iter()
        .filter(|run| run.scenario == scenario && run.split == split)
        .count()
}

#[test]
fn the_suite_expands_to_two_hundred_twenty_runs_split_evenly() {
    let runs = real_runs();
    assert_eq!(runs.len(), 220);
    assert_eq!(
        runs.iter().filter(|run| run.split == Split::Dev).count(),
        110
    );
    assert_eq!(
        runs.iter()
            .filter(|run| run.split == Split::Holdout)
            .count(),
        110
    );
}

#[test]
fn every_scenario_has_its_declared_seeds() {
    let runs = real_runs();
    let per_scenario = [
        ("default", 24),
        ("target", 18),
        ("flat-hazard", 4),
        ("late-onset", 4),
        ("idle-limit", 4),
        ("misleading-mce", 4),
        ("target-nonmember-mce", 4),
        ("shared-voltage", 12),
        ("target-shared-voltage", 12),
        ("target-r7-vf-boost", 12),
        ("target-r7-request-gap", 12),
    ];
    let scenarios: Vec<&str> = runs
        .iter()
        .map(|run| run.scenario.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(scenarios.len(), per_scenario.len());
    for (name, seeds) in per_scenario {
        assert_eq!(count(&runs, name, Split::Dev), seeds, "{name} dev");
        assert_eq!(count(&runs, name, Split::Holdout), seeds, "{name} holdout");
        let dev: Vec<u64> = runs
            .iter()
            .filter(|run| run.scenario == name && run.split == Split::Dev)
            .map(|run| run.seed)
            .collect();
        assert_eq!(
            dev,
            (1..=seeds as u64).collect::<Vec<_>>(),
            "{name} dev seeds"
        );
        let holdout: Vec<u64> = runs
            .iter()
            .filter(|run| run.scenario == name && run.split == Split::Holdout)
            .map(|run| run.seed)
            .collect();
        assert_eq!(
            holdout,
            (101..=100 + seeds as u64).collect::<Vec<_>>(),
            "{name} holdout seeds"
        );
    }
}

#[test]
fn replay_is_on_for_the_target_scenarios_only() {
    let runs = real_runs();
    let replaying: BTreeSet<&str> = runs
        .iter()
        .filter(|run| run.replay)
        .map(|run| run.scenario.as_str())
        .collect();
    assert_eq!(
        replaying,
        BTreeSet::from([
            "target",
            "target-nonmember-mce",
            "target-shared-voltage",
            "target-r7-vf-boost",
            "target-r7-request-gap",
        ])
    );
}

#[test]
fn run_ids_are_unique_and_filesystem_safe() {
    let runs = real_runs();
    let ids: BTreeSet<&str> = runs.iter().map(|run| run.id.as_str()).collect();
    assert_eq!(ids.len(), runs.len());
    for run in &runs {
        let (scenario, leaf) = run.id.as_str().split_once('/').unwrap();
        assert_eq!(scenario, run.scenario);
        assert_eq!(leaf, format!("{}-{}", run.split, run.seed));
        assert!(
            run.id
                .as_str()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')),
            "{}",
            run.id
        );
    }
}

#[test]
fn target_members_take_their_seeds_by_index_within_each_split() {
    let runs = real_runs();
    let mut by_member: BTreeMap<(Split, String), Vec<u64>> = BTreeMap::new();
    for run in runs.iter().filter(|run| run.scenario == "target") {
        let machine = run.machine.as_ref().unwrap().file_name().unwrap();
        by_member
            .entry((run.split, machine.to_string_lossy().into_owned()))
            .or_default()
            .push(run.seed);
    }
    assert_eq!(by_member.len(), 18);
    for member in 0..9_u64 {
        let file = format!("target-fit-{member}.json");
        assert_eq!(
            by_member[&(Split::Dev, file.clone())],
            vec![member + 1, member + 10],
            "dev member {member}"
        );
        assert_eq!(
            by_member[&(Split::Holdout, file)],
            vec![member + 101, member + 110],
            "holdout member {member}"
        );
    }
}

#[test]
fn smoke_marks_one_run_per_scenario() {
    let runs = real_runs();
    let smoke: Vec<(&str, u64)> = runs
        .iter()
        .filter(|run| run.smoke)
        .map(|run| (run.scenario.as_str(), run.seed))
        .collect();
    assert_eq!(
        smoke,
        vec![
            ("default", 122),
            ("target", 115),
            ("flat-hazard", 104),
            ("late-onset", 101),
            ("idle-limit", 103),
            ("misleading-mce", 104),
            ("target-nonmember-mce", 103),
            ("shared-voltage", 103),
            ("target-shared-voltage", 104),
            ("target-r7-vf-boost", 110),
            ("target-r7-request-gap", 10),
        ]
    );
}

#[test]
fn every_machine_the_suite_names_loads() {
    let runs = real_runs();
    let machines: BTreeSet<&Path> = runs
        .iter()
        .filter_map(|run| run.machine.as_deref())
        .collect();
    assert_eq!(machines.len(), 18);
    for machine in machines {
        togi_sim::file::load(machine)
            .unwrap_or_else(|error| panic!("{}: {error}", machine.display()));
    }
    assert!(
        runs.iter()
            .filter(|run| run.scenario == "default")
            .all(|run| run.machine.is_none())
    );
}
