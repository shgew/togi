use std::collections::BTreeSet;
use std::path::Path;

use togi_config::Config;
use togi_journal::{Payload, StateDir};
use togi_sim::History;

use crate::HistoryError;

#[expect(
    clippy::disallowed_methods,
    reason = "reading a state directory's history takes its lock without syncing anything"
)]
fn locked(dir: &Path) -> Result<StateDir, HistoryError> {
    Ok(StateDir::lock_unsynced(dir)?)
}

/// What the journals of the state directory at `dir` say earlier invocations did: the boots they ran, the wall time
/// they reached and the BIOS context of the newest session that recorded one.
pub fn history(dir: &Path) -> Result<History, HistoryError> {
    history_in(&locked(dir)?)
}

/// [`history`] over a state directory the caller already holds.
pub fn history_in(dir: &StateDir) -> Result<History, HistoryError> {
    let mut boots = BTreeSet::new();
    let mut last_time = None;
    let mut bios_context = None;
    for recording in dir.recordings()? {
        for event in recording.events() {
            boots.insert(event.boot.clone());
            last_time = last_time.max(Some(event.time));
            if let Payload::SessionContext(context) = &event.payload {
                bios_context = Some(context.bios_context());
            }
        }
    }
    Ok(History {
        boots: u32::try_from(boots.len()).unwrap_or(u32::MAX),
        last_time,
        bios_context,
    })
}

/// The configuration a resume of `dir` runs under: the one the latest `config.loaded` recorded, standing in for the
/// file `togi run` would load; `fresh` when no journal recorded one. A recorded configuration this build would
/// refuse as a file is an error.
pub fn recorded_config(dir: &Path, fresh: Config) -> Result<Config, HistoryError> {
    recorded_config_in(&locked(dir)?, fresh)
}

/// [`recorded_config`] over a state directory the caller already holds.
pub fn recorded_config_in(dir: &StateDir, fresh: Config) -> Result<Config, HistoryError> {
    let mut recorded = fresh;
    for recording in dir.recordings()? {
        for event in recording.events() {
            if let Payload::ConfigLoaded(loaded) = &event.payload {
                recorded = Config::from_snapshot(&loaded.config)?;
            }
        }
    }
    Ok(recorded)
}
