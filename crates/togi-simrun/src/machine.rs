//! What the simulated machine keeps between processes, as `machine.json` in the state directory: a capped
//! simulation leaves it, and the next process's simulator continues from it, as real hardware keeps its clock, its
//! kernel logs and the reset it is about to report across a restart of the tool.

use std::io;
use std::path::{Path, PathBuf};

use togi_journal::StateDir;
use togi_journal::fs::Fs;
use togi_sim::{Model, Persisted, RestoreError, RunSettings, Simulator};

use crate::input::Persistence;
use crate::simulate::lock;
use crate::{HistoryError, SimError, history_in};

const MACHINE_FILE: &str = "machine.json";

/// Why no simulator could continue a state directory's simulation.
#[derive(Debug, thiserror::Error)]
pub enum ResumeError {
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error("read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("decode {}: {source}", path.display())]
    Decode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("restore the simulated machine: {0}")]
    Restore(#[from] RestoreError),
}

/// The simulator a later process continues a simulation in `persistence` with: the machine a capped simulation
/// left there, which keeps its own seed, else a fresh machine for `model` and `seed` that starts after the boots and
/// wall time the journals recorded.
///
/// # Errors
///
/// A state directory that cannot be locked or read, a `machine.json` that cannot be decoded, or one for another
/// machine.
pub fn resume_simulator(
    persistence: &Persistence,
    model: Model,
    seed: u64,
    facts: Option<togi_sim::replay::Replay>,
) -> Result<Simulator, ResumeError> {
    let dir = lock(persistence).map_err(HistoryError::from)?;
    let path = dir.path().join(MACHINE_FILE);
    let saved = read(persistence, &path).map_err(|source| ResumeError::Read {
        path: path.clone(),
        source,
    })?;
    match saved {
        Some(bytes) => {
            let persisted = serde_json::from_slice(&bytes)
                .map_err(|source| ResumeError::Decode { path, source })?;
            Ok(Simulator::restore(model, facts, persisted)?)
        }
        None => Ok(Simulator::new(
            model,
            RunSettings {
                seed,
                history: history_in(&dir)?,
                replay: facts,
            },
        )),
    }
}

fn read(persistence: &Persistence, path: &Path) -> io::Result<Option<Vec<u8>>> {
    let read = match persistence {
        Persistence::Memory(fs) => Fs::read(fs, path),
        Persistence::Files(_) => std::fs::read(path),
    };
    match read {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Removes the machine an earlier capped simulation left, which the simulation now running supersedes.
pub(crate) fn forget(persistence: &Persistence, dir: &StateDir) -> Result<(), SimError> {
    let path = dir.path().join(MACHINE_FILE);
    let removed = match persistence {
        Persistence::Memory(fs) => fs.remove_file(&path),
        Persistence::Files(_) => std::fs::remove_file(&path),
    };
    match removed {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(SimError::Machine {
            path,
            source: error,
        }),
        _ => Ok(()),
    }
}

/// Leaves `machine` for the next process to continue from.
pub(crate) fn save(
    persistence: &Persistence,
    dir: &StateDir,
    machine: &Persisted,
) -> Result<(), SimError> {
    let path = dir.path().join(MACHINE_FILE);
    let mut bytes = serde_json::to_vec_pretty(machine).map_err(SimError::EncodeMachine)?;
    bytes.push(b'\n');
    match persistence {
        Persistence::Memory(fs) => {
            fs.put(&path, bytes);
            Ok(())
        }
        Persistence::Files(_) => {
            std::fs::write(&path, bytes).map_err(|source| SimError::Machine { path, source })
        }
    }
}
