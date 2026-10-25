use std::cell::RefCell;
use std::io;
use std::num::NonZeroU32;
use std::path::Path;
use std::rc::Rc;

use togi_journal::{
    Event, FileError, Journal, LockError, LookupError, MemFs, ReadError, Recording, StateDir,
};
use togi_session::{
    Ended, PublicationPolicy, ResumeCapsule, RunInput, SessionError, prepare, prepare_kept,
};

use crate::input::{Finished, Persistence, Replay, SimInput, SimOutcome};
use crate::observe::{SharedLog, Slot, Tee, UserLog, hook};
use crate::{HistoryError, SimError, machine};

#[expect(
    clippy::disallowed_methods,
    reason = "a simulation keeps its state directory in memory or on disk without syncing"
)]
pub(crate) fn lock(persistence: &Persistence) -> Result<StateDir, LockError> {
    Ok(match persistence {
        Persistence::Memory(fs) => StateDir::simulated(fs.clone())?,
        Persistence::Files(path) => StateDir::lock_unsynced(path)?,
    })
}

/// Boots the simulated machine, runs the session on it and reboots it after every crash, until a run ends without
/// one or `max_boots` boots have run.
///
/// # Errors
///
/// A state directory that cannot be locked, opened, closed or dumped, a resume capsule that differs from a fresh
/// fold of its journal, or a boot that left none to check.
pub fn simulate(input: SimInput) -> Result<SimOutcome, SimError> {
    simulate_checked(input, &mut verify)
}

type Check<'a> =
    dyn FnMut(Option<&ResumeCapsule>, &[Event], bool, u32) -> Result<(), SimError> + 'a;

/// [`simulate`], checking each due boot's resume state with `check`.
#[expect(
    clippy::disallowed_methods,
    reason = "a simulation writes its in-memory state directory to disk only on request"
)]
fn simulate_checked(input: SimInput, check: &mut Check<'_>) -> Result<SimOutcome, SimError> {
    simulate_with_dump(input, check, &mut |fs, destination| fs.dump_to(destination))
}

fn simulate_with_dump(
    input: SimInput,
    check: &mut Check<'_>,
    dump: &mut dyn FnMut(&MemFs, &Path) -> io::Result<()>,
) -> Result<SimOutcome, SimError> {
    let SimInput {
        config,
        source,
        build,
        simulator,
        persistence,
        replay,
        verify_every,
        max_boots,
        stop_at,
        write_samples,
        defects,
        log,
        renderer,
        zone,
        cancel,
        observer,
        dump_to,
    } = input;
    let memory = match &persistence {
        Persistence::Memory(fs) => Some(fs.clone()),
        Persistence::Files(_) => None,
    };
    let samples = match (&persistence, write_samples) {
        (_, false) => None,
        (Persistence::Memory(_), true) => return Err(SimError::SamplesInMemory),
        (Persistence::Files(_), true) => Some(lock(&persistence)?.trials_dir()),
    };
    simulator.set_samples_dir(samples);
    let user: SharedLog = Rc::new(RefCell::new(UserLog::new(log)));
    let slot: Slot = Rc::new(RefCell::new(observer));
    let mut resume: Option<ResumeCapsule> = None;
    let mut kept: Option<Journal> = None;
    let mut outcome = None;
    let mut superseded = false;
    for number in 1..=max_boots.get() {
        let mut machine = simulator.seams();
        let identity = machine.host.boot_id();
        let boot = match identity {
            Ok(boot) => boot,
            Err(source) => {
                return Err(abandon(
                    kept.take(),
                    resume.as_ref(),
                    SimError::Host {
                        boot: number,
                        source,
                    },
                ));
            }
        };
        let now = simulator.now();
        let reopened = kept.is_some();
        let prepared = match kept.take() {
            Some(mut journal) => {
                if journal.reboot(boot.clone()).is_ok() {
                    prepare_kept(journal, &build, boot.clone(), now)
                } else {
                    // The flush a reboot starts with failed and poisoned the journal: reopen the directory from what
                    // the file holds, after publishing the state the crashed boot left pending, for the reopened
                    // boot to compare with.
                    let mut dir = close(journal, number)?;
                    if let Some(capsule) = &resume {
                        publish(capsule, &mut dir, number)?;
                    }
                    prepare(dir, &build, boot.clone(), now)
                }
            }
            None => prepare(lock(&persistence)?, &build, boot.clone(), now),
        }
        .map_err(|source| {
            let error = SimError::Prepare {
                boot: number,
                source,
            };
            match &resume {
                Some(capsule) if reopened => republish(&persistence, capsule, error),
                _ => error,
            }
        })?;
        let journal = prepared.journal;
        let opened = journal.events().len();
        if journal.events().is_empty() {
            simulator.forget_last_trial();
        }
        if let Some(event) = journal.repaired().and_then(|seq| journal.event(seq)) {
            user.borrow_mut().write_event(renderer, event, &zone);
        }
        let run_cancel = cancel.child_token();
        if resume.is_none() && replay == Replay::Warm {
            resume = ResumeCapsule::read_snapshot(&journal, defects);
        }
        let mut ended = togi_session::run(RunInput {
            config: config.clone(),
            source: source.clone(),
            build: build.clone(),
            boot,
            journal,
            machine,
            publication: PublicationPolicy::OnFinish,
            bootloader: None,
            defects,
            resume: resume.take(),
            import: prepared.import,
            log: Some(Box::new(Tee(Rc::clone(&user)))),
            on_append: Some(hook(Rc::clone(&slot), run_cancel.clone(), stop_at)),
            renderer,
            zone: zone.clone(),
            cancel: run_cancel,
            report: Box::new(io::sink()),
        });
        if let Some(panic) = ended.panic.take() {
            // The journal has no drop flush: close it so a file-backed boot keeps the events it buffered and the
            // writer lock is released. A close failure is not reported over the panic being resumed.
            drop(ended.journal.close());
            panic.resume();
        }
        let crash = ended.crashed();
        let appended = ended.journal.events().len() > opened;
        if due(number, max_boots, crash, verify_every) {
            let checked = check(
                ended.capsule.as_ref(),
                ended.journal.events(),
                !crash && !ended.errors.is_empty(),
                number,
            );
            if let Err(error) = checked {
                let pending = ended.capsule.as_ref().filter(|_| crash);
                return Err(abandon(Some(ended.journal), pending, error));
            }
        }
        let Ended {
            stop,
            mut errors,
            emergency,
            journal,
            capsule,
            panic: _,
        } = ended;
        errors.extend(user.borrow_mut().take_failure().map(SessionError::Log));
        let failed = crash && errors.iter().any(|error| !error.is_crash());
        // A warm reboot carries the open journal and the folds into the next boot, which adopts them, so the crashed
        // boot leaves `state.json` pending until a boot ends the simulation; the last boot's capsule is published
        // below, as is every cold boot's, which nothing adopts.
        let keeps = crash
            && !failed
            && replay == Replay::Warm
            && number < max_boots.get()
            && capsule
                .as_ref()
                .is_some_and(|capsule| capsule.covers(&journal, defects));
        if keeps {
            if appended && !superseded {
                if let Err(error) = machine::forget(&persistence, journal.dir()) {
                    return Err(abandon(Some(journal), capsule.as_ref(), error));
                }
                superseded = true;
            }
            kept = Some(journal);
            resume = capsule;
            if let Err(source) = simulator.reboot() {
                return Err(abandon(
                    kept.take(),
                    resume.as_ref(),
                    SimError::Reboot {
                        boot: number,
                        source,
                    },
                ));
            }
            continue;
        }
        let mut dir = close(journal, number)?;
        if crash && let Some(capsule) = &capsule {
            publish(capsule, &mut dir, number)?;
        }
        if appended && !superseded {
            machine::forget(&persistence, &dir)?;
            superseded = true;
        }
        if !crash {
            outcome = Some(SimOutcome::Stopped(Box::new(Finished {
                stop,
                errors,
                emergency,
                capsule,
            })));
            break;
        }
        if failed {
            return Err(SimError::Failed {
                boot: number,
                errors: errors
                    .into_iter()
                    .filter(|error| !error.is_crash())
                    .collect(),
            });
        }
        resume = capsule.filter(|_| replay == Replay::Warm);
        simulator.reboot().map_err(|source| SimError::Reboot {
            boot: number,
            source,
        })?;
    }
    user.borrow_mut().flush().map_err(SimError::FlushLog)?;
    if outcome.is_none() {
        machine::save(&persistence, &lock(&persistence)?, &simulator.persisted())?;
    }
    if let Some(destination) = dump_to {
        dump_with(memory.as_ref(), &destination, dump)?;
    }
    Ok(outcome.unwrap_or(SimOutcome::Capped { boots: max_boots }))
}

/// Whether boot `number` checks its resume state: every boot that ends the simulation, by a stop or by the cap, and
/// every `every`th boot.
fn due(number: u32, max_boots: NonZeroU32, crashed: bool, every: Option<NonZeroU32>) -> bool {
    !crashed
        || number == max_boots.get()
        || every.is_some_and(|every| number.is_multiple_of(every.get()))
}

/// `latched` is a run that ended on a journal failure other than a crash, which leaves no capsule.
fn verify(
    capsule: Option<&ResumeCapsule>,
    events: &[Event],
    latched: bool,
    boot: u32,
) -> Result<(), SimError> {
    match capsule {
        Some(capsule) => capsule
            .verify(events)
            .map_err(|source| SimError::Diverged { boot, source }),
        None if latched => Ok(()),
        None => Err(SimError::NoCapsule { boot }),
    }
}

fn close(journal: Journal, boot: u32) -> Result<StateDir, SimError> {
    journal
        .close()
        .map_err(|source| SimError::Close { boot, source })
}

/// Ends a simulation on an error: closes the open journal, if any, after publishing the state its crashed boot left
/// pending, as a normal exit does. `error` stays the one returned; a failure here is not reported over it.
fn abandon(journal: Option<Journal>, pending: Option<&ResumeCapsule>, error: SimError) -> SimError {
    if let Some(journal) = journal
        && let Ok(mut dir) = journal.close()
        && let Some(capsule) = pending
    {
        drop(capsule.publish(&mut dir));
    }
    error
}

/// [`abandon`] for a journal that [`prepare_kept`] consumed: publishes the pending state into the directory unless
/// it no longer holds the journal, as when the failed preparation archived it, which took the state file along.
fn republish(persistence: &Persistence, pending: &ResumeCapsule, error: SimError) -> SimError {
    if let Ok(mut dir) = lock(persistence)
        && matches!(dir.live_session(), Ok(Some(_)))
    {
        drop(pending.publish(&mut dir));
    }
    error
}

fn publish(capsule: &ResumeCapsule, dir: &mut StateDir, boot: u32) -> Result<(), SimError> {
    capsule
        .publish(dir)
        .map_err(|source| SimError::Publish { boot, source })
}

fn dump_with(
    memory: Option<&MemFs>,
    destination: &Path,
    dump: &mut dyn FnMut(&MemFs, &Path) -> io::Result<()>,
) -> Result<(), SimError> {
    match memory {
        Some(fs) => dump(fs, destination).map_err(SimError::Dump),
        None => Ok(()),
    }
}

/// The events of the live journal in `persistence`, after a simulation. Archives are not read, so one this build
/// cannot decode does not hide the live session.
pub fn events(persistence: &Persistence) -> Result<Vec<Event>, HistoryError> {
    let live = lock(persistence)?.path().join("events.jsonl");
    let read = match persistence {
        Persistence::Memory(fs) => Recording::read_in(fs, &live),
        Persistence::Files(_) => Recording::read(&live),
    };
    match read {
        Ok(recording) => Ok(recording.into_events()),
        Err(FileError {
            source: ReadError::Io(error),
            ..
        }) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(LookupError::from(error).into()),
    }
}

#[cfg(test)]
mod tests;
