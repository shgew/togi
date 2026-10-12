use std::collections::HashMap;
use std::path::Path;

use togi_journal::{Build, Event, Payload, Seq, SmuIntentReceipt, Stamp, StateDir};

use crate::error::InstallError;
use crate::{Error, Scenario};

/// Writes the scenario's journal as `events.jsonl` in `dir` through the journal's own writer, one open journal per
/// boot, and returns the build its session started under.
pub fn install(dir: &Path, scenario: Scenario) -> Result<Build, Error> {
    let events = scenario.events()?;
    write(dir, &events).map_err(|source| Error::Install {
        dir: dir.to_path_buf(),
        source,
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "test-support state directories need not survive a power loss, so the journal skips fsync"
)]
fn lock(dir: &Path) -> Result<StateDir, togi_journal::LockError> {
    StateDir::lock_unsynced(dir)
}

fn write(dir: &Path, events: &[Event]) -> Result<Build, InstallError> {
    let Some(Payload::SessionStart(start)) = events.first().map(|event| &event.payload) else {
        return Err(InstallError::NoSession);
    };
    let build = start.build.clone();
    let mut state = lock(dir)?;
    for boot in events.chunk_by(|a, b| a.boot == b.boot) {
        let now = boot[0].time;
        let mut journal = state.open_at(boot[0].boot.clone(), now)?;
        let mut intents: HashMap<Seq, SmuIntentReceipt> = HashMap::new();
        for event in boot {
            let stamp = Stamp {
                time: event.time,
                mono: event.mono,
            };
            let failed = |source| InstallError::Append {
                kind: event.kind().as_str(),
                seq: event.seq.get(),
                source,
            };
            match &event.payload {
                Payload::SmuIntent(record) => {
                    let receipt = journal
                        .append_smu_intent(stamp, record.command(), &event.cause)
                        .map_err(failed)?;
                    intents.insert(event.seq, receipt);
                }
                Payload::SmuWrite(_) => {
                    let receipt = event
                        .cause
                        .iter()
                        .find_map(|cause| intents.remove(cause))
                        .ok_or(InstallError::OrphanWrite {
                            seq: event.seq.get(),
                        })?;
                    journal
                        .append_smu_write(stamp, receipt)
                        .map(drop)
                        .map_err(failed)?;
                }
                payload => {
                    journal
                        .append(stamp, payload.clone(), &event.cause)
                        .map_err(failed)?;
                }
            }
        }
        state = journal.close()?;
    }
    drop(state);
    Ok(build)
}
