use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use togi_machine::CoreSet;
use togi_machine::{Regime, TargetOffset, pick_workload};
use togi_sim::definition::CoreDef;
use togi_sim::{InvalidMachine, Load, MachineDefinition, Model, file};

pub(crate) fn machines_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench/machines")
}

pub(crate) fn committed() -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(machines_dir())
        .expect("machines directory")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    paths
}

pub(crate) fn core_def(id: i64, alone: i64, together: i64) -> CoreDef {
    CoreDef {
        id,
        alone: [alone; 5],
        together: [together; 7],
        flat: 0.0,
        idle: None,
        workload: BTreeMap::new(),
    }
}

pub(crate) fn flat_cores(cores: i64, alone: i64, together: i64) -> Vec<CoreDef> {
    (0..cores).map(|id| core_def(id, alone, together)).collect()
}

pub(crate) fn machine(cores: i64, core: Vec<CoreDef>) -> MachineDefinition {
    MachineDefinition {
        cores,
        core,
        ..MachineDefinition::default()
    }
}

pub(crate) fn model(definition: &MachineDefinition) -> Model {
    Model::new(definition, 1).expect("a valid machine")
}

pub(crate) fn invalid(definition: &MachineDefinition) -> InvalidMachine {
    Model::new(definition, 1).expect_err("an invalid machine")
}

pub(crate) fn offsets(values: &[i64]) -> Vec<TargetOffset> {
    values
        .iter()
        .map(|&value| TargetOffset::new(value).expect("an offset in range"))
        .collect()
}

pub(crate) fn cores(ids: &[i64]) -> CoreSet {
    ids.iter()
        .map(|&id| togi_machine::CoreId::new(id).expect("a core ID"))
        .collect()
}

pub(crate) fn load<'a>(regime: Regime, workload: &'a str, ids: &[i64], seconds: f64) -> Load<'a> {
    Load {
        regime,
        workload,
        cores: cores(ids),
        duration: Duration::from_secs_f64(seconds),
    }
}

pub(crate) fn committed_machine(name: &str) -> MachineDefinition {
    file::load(machines_dir().join(name)).expect("a committed machine")
}

pub(crate) fn r7_workload(index: usize) -> &'static str {
    pick_workload(Regime::R7, index).id
}

pub(crate) fn machine_dir_text(name: &str) -> String {
    std::fs::read_to_string(machines_dir().join(name)).expect("a committed machine")
}
