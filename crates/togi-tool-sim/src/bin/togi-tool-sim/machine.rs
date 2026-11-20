use std::path::Path;

use togi_sim::replay::Replay;
use togi_sim::{InvalidMachine, MachineDefinition, MachineSource, Model, file};
use togi_tool_sim::scripted::developer_machine;
use togi_tool_trialfacts::{Extracts, ExtractsError};

#[derive(Debug, thiserror::Error)]
pub(crate) enum LoadError {
    #[error("load machine")]
    File(#[from] file::LoadError),
    #[error("load machine")]
    Invalid(#[from] InvalidMachine),
    #[error(
        "replaying facts needs a machine file naming a facts extract; the default machine has none"
    )]
    ReplayWithoutMachine,
    #[error(transparent)]
    Replay(#[from] ExtractsError),
}

/// The standalone session defaults to a script. Explicit research machines use the suite's fitted loader.
pub(crate) fn load_session(
    path: Option<&Path>,
    seed: u64,
    replay: bool,
    read: impl FnOnce(&Path) -> Result<MachineDefinition, file::LoadError>,
) -> Result<MachineSource, LoadError> {
    match (path, replay) {
        (None, false) => Ok(MachineSource::Scripted(developer_machine())),
        (None, true) => Err(LoadError::ReplayWithoutMachine),
        (Some(path), replay) => {
            let (model, replay) = load_with(Some(path), seed, replay, &mut Extracts::new(), read)?;
            Ok(MachineSource::fitted(model, replay))
        }
    }
}

/// The suite's seeded research model and its replay oracle when asked for. Nothing here touches a state directory.
pub(crate) fn load(
    path: Option<&Path>,
    seed: u64,
    replay: bool,
    extracts: &mut Extracts,
) -> Result<(Model, Option<Replay>), LoadError> {
    load_with(path, seed, replay, extracts, |path| file::load(path))
}

pub(crate) fn load_with(
    path: Option<&Path>,
    seed: u64,
    replay: bool,
    extracts: &mut Extracts,
    read: impl FnOnce(&Path) -> Result<MachineDefinition, file::LoadError>,
) -> Result<(Model, Option<Replay>), LoadError> {
    let definition = match path {
        Some(path) => read(path)?,
        None => MachineDefinition::default(),
    };
    let replay = match (replay, path) {
        (false, _) => None,
        (true, None) => return Err(LoadError::ReplayWithoutMachine),
        (true, Some(path)) => Some(extracts.replay(path, &definition)?),
    };
    Ok((Model::new(&definition, seed)?, replay))
}
