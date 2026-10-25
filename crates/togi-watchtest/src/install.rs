use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use togi_journal::fs::Fs;
use togi_journal::{Build, DirError, Event, Payload, Seq, SmuIntentReceipt, Stamp, StateDir};

use crate::error::InstallError;
use crate::{Error, Scenario};

/// Writes the scenario's journal as `events.jsonl` in `dir` through the journal's own writer, kept open across each
/// recorded reboot as the simulator keeps it, so no boot re-reads the journal written so far, and returns the build its session started under. A directory that already holds a journal is refused, since
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
    let Some((Payload::SessionStart(start), start_boot, start_time)) = events
        .first()
        .map(|event| (&event.payload, event.boot.clone(), event.time))
    else {
        return Err(InstallError::NoSession);
    };
    let build = start.build.clone();
    #[expect(
        clippy::disallowed_methods,
        reason = "fixture installers explicitly supply their memory or non-durable filesystem"
    )]
    let state = match &fs {
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
    let mut journal = state.open_at(start_boot, start_time)?;
    for (index, boot) in events.chunk_by(|a, b| a.boot == b.boot).enumerate() {
        if index > 0 {
            journal
                .reboot(boot[0].boot.clone())
                .map_err(|source| InstallError::Reboot {
                    seq: boot[0].seq.get(),
                    source,
                })?;
        }
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
                payload @ (Payload::SessionStart(_)
                | Payload::SessionContext(_)
                | Payload::SessionBaseline(_)
                | Payload::SessionNotice(_)
                | Payload::SessionWarning(_)
                | Payload::SessionArchived(_)
                | Payload::SessionCarried(_)
                | Payload::ConfigLoaded(_)
                | Payload::PreflightCheck(_)
                | Payload::SmuReadback(_)
                | Payload::SmuError(_)
                | Payload::ProfileApplied(_)
                | Payload::ProfileChange(_)
                | Payload::ProfileRestored(_)
                | Payload::TrialIntent(_)
                | Payload::TrialStart(_)
                | Payload::TrialProgress(_)
                | Payload::TrialSignal(_)
                | Payload::TrialSample(_)
                | Payload::TrialEnd(_)
                | Payload::TrialCarried(_)
                | Payload::Failure(_)
                | Payload::FailureCarried(_)
                | Payload::Mce(_)
                | Payload::CrashDetected(_)
                | Payload::TunerDecision(_)
                | Payload::CorePhase(_)
                | Payload::CheckingCycle(_)
                | Payload::CheckingStep(_)
                | Payload::CheckingChain(_)
                | Payload::HostRanking(_)
                | Payload::HuntStart(_)
                | Payload::HuntGroup(_)
                | Payload::HuntEnd(_)
                | Payload::HuntSkipped(_)
                | Payload::Combination(_)
                | Payload::DeepeningRound(_)
                | Payload::TunerWarning(_)
                | Payload::BackendRetry(_)
                | Payload::CommandReset(_)
                | Payload::DefectFound(_)
                | Payload::DefectAnswered(_)
                | Payload::DeadEnd(_)
                | Payload::BootSavedEntry(_)
                | Payload::BootLeaveReason(_)
                | Payload::Shutdown(_)
                | Payload::JournalTorn(_)
                | Payload::DayBootStart(_)
                | Payload::DayCheckpoint(_)
                | Payload::DayOffsetsChanged(_)
                | Payload::DayCleanStop(_)
                | Payload::DayAnswer(_)
                | Payload::DayRecord(_)
                | Payload::DayFault(_)
                | Payload::SessionImported(_)
                | Payload::ExposureObserved(_)
                | Payload::CcdEdge(_)
                | Payload::CcdCap(_)
                | Payload::NightPlan(_)
                | Payload::NightQualification(_)
                | Payload::NightReproduction(_)
                | Payload::DayStep(_)
                | Payload::DayApplied(_)
                | Payload::DayEvent(_)
                | Payload::DayVeto(_)
                | Payload::DayHold(_)
                | Payload::DayFreeze(_)
                | Payload::DayInterval(_)
                | Payload::DayImported(_)
                | Payload::DayUncleanEnd(_)
                | Payload::StateRebuilt(_)) => {
                    journal
                        .append(stamp, payload.clone(), &event.cause)
                        .map_err(failed)?;
                }
            }
        }
    }
    drop(journal.close()?);
    Ok(build)
}
