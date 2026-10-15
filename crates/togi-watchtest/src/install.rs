use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use togi_journal::fs::Fs;
use togi_journal::{Build, DirError, Event, Payload, Seq, SmuIntentReceipt, Stamp, StateDir};

use crate::error::InstallError;
use crate::{Error, Scenario};

/// Writes the scenario's journal as `events.jsonl` in `dir` through the journal's own writer, one open journal per
/// boot, and returns the build its session started under. A directory that already holds a journal is refused, since
/// the writer would append the scenario after it.
pub fn install(dir: &Path, scenario: Scenario) -> Result<Build, Error> {
    install_events(dir, &scenario.events()?)
}

/// [`install`] for a journal the caller simulated, such as with [`crate::simulate`].
pub fn install_events(dir: &Path, events: &[Event]) -> Result<Build, Error> {
    install_events_in(dir, events, None)
}

/// [`install`] using the supplied filesystem, including its durability guarantees.
pub fn install_with_fs(fs: Arc<dyn Fs>, dir: &Path, scenario: Scenario) -> Result<Build, Error> {
    install_events_with_fs(fs, dir, &scenario.events()?)
}

/// [`install_events`] using the supplied filesystem, including its durability guarantees.
pub fn install_events_with_fs(
    fs: Arc<dyn Fs>,
    dir: &Path,
    events: &[Event],
) -> Result<Build, Error> {
    install_events_in(dir, events, Some(fs))
}

fn install_events_in(
    dir: &Path,
    events: &[Event],
    fs: Option<Arc<dyn Fs>>,
) -> Result<Build, Error> {
    write(dir, events, fs).map_err(|source| Error::Install {
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

fn write(dir: &Path, events: &[Event], fs: Option<Arc<dyn Fs>>) -> Result<Build, InstallError> {
    let Some(Payload::SessionStart(start)) = events.first().map(|event| &event.payload) else {
        return Err(InstallError::NoSession);
    };
    let build = start.build.clone();
    #[expect(
        clippy::disallowed_methods,
        reason = "fixture installers explicitly supply their memory or non-durable filesystem"
    )]
    let mut state = match &fs {
        Some(fs) => StateDir::lock_with_fs(Arc::clone(fs), dir)?,
        None => lock(dir)?,
    };
    let journal_path = state.path().join("events.jsonl");
    let exists = match fs {
        Some(fs) => fs
            .kind(&journal_path)
            .map_err(|source| DirError::Io {
                op: "stat",
                path: journal_path.clone(),
                source,
            })?
            .is_some(),
        None => journal_path.exists(),
    };
    if exists {
        return Err(InstallError::NotEmpty);
    }
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
