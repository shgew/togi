use std::collections::BTreeSet;
use std::path::Path;

use togi_config::Config;
use togi_journal::{Payload, StateDir};
use togi_sim::History;

use crate::{HistoryError, Persistence};

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
        boots: recorded_boot_count(boots.len())?,
        last_time,
        bios_context,
    })
}

/// The configuration a resume of `persistence` runs under: the one the latest `config.loaded` recorded, standing in for the
/// file `togi run` would load; `fresh` when no journal recorded one. A recorded configuration this build would
/// refuse as a file is an error.
pub fn recorded_config(persistence: &Persistence, fresh: Config) -> Result<Config, HistoryError> {
    recorded_config_in(
        &crate::simulate::lock(persistence).map_err(HistoryError::from)?,
        fresh,
    )
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

fn recorded_boot_count(count: usize) -> Result<u32, HistoryError> {
    u32::try_from(count).map_err(HistoryError::BootCount)
}

#[cfg(test)]
mod numeric_tests {
    use super::recorded_boot_count;

    #[test_case::test_case(0, Some(0); "no boots")]
    #[test_case::test_case(u32::MAX as usize, Some(u32::MAX); "largest exact boot count")]
    #[test_case::test_case(u32::MAX as usize + 1, None; "first overflowing boot count")]
    #[cfg(target_pointer_width = "64")]
    fn history_boot_counts_are_exact(count: usize, expected: Option<u32>) {
        assert_eq!(recorded_boot_count(count).ok(), expected);
    }
}
