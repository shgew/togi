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
fn simulate_checked(input: SimInput, check: &mut Check<'_>) -> Result<SimOutcome, SimError> {
    let SimInput {
        config,
        source,
        build,
        simulator,
        persistence,
        replay,
        verify_every,
        max_boots,
        policy,
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
        let identity = machine
            .host
            .boot_id()
            .and_then(|boot| Ok((boot, machine.host.bios_context()?)));
        let (boot, context) = match identity {
            Ok(identity) => identity,
            Err(source) => {
                release(kept.take());
                return Err(SimError::Host {
                    boot: number,
                    source,
                });
            }
        };
        let now = simulator.now();
        let prepared = match kept.take() {
            Some(mut journal) => {
                if journal.reboot(boot.clone()).is_ok() {
                    prepare_kept(journal, &build, boot.clone(), now, Some(&context), defects)
                } else {
                    // The flush a reboot starts with failed and poisoned the journal: reopen the directory from what
                    // the file holds, after publishing the state the crashed boot left pending, for the reopened
                    // boot to compare with.
                    let mut dir = close(journal, number)?;
                    if let Some(capsule) = &resume {
                        publish(capsule, &mut dir, number)?;
                    }
                    prepare(dir, &build, boot.clone(), now, Some(&context), defects)
                }
            }
            None => prepare(
                lock(&persistence)?,
                &build,
                boot.clone(),
                now,
                Some(&context),
                defects,
            ),
        }
        .map_err(|source| SimError::Prepare {
            boot: number,
            source,
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
        let ended = togi_session::run(RunInput {
            config: config.clone(),
            source: source.clone(),
            build: build.clone(),
            boot,
            journal,
            machine,
            policy,
            publication: PublicationPolicy::OnFinish,
            bootloader: None,
            prompt: None,
            defects,
            resume: resume.take(),
            carry: prepared.carry,
            log: Some(Box::new(Tee(Rc::clone(&user)))),
            on_append: Some(hook(Rc::clone(&slot), run_cancel.clone())),
            renderer,
            zone: zone.clone(),
            cancel: run_cancel,
            report: Box::new(io::sink()),
        });
        let crash = ended.crashed();
        let appended = ended.journal.events().len() > opened;
        if due(number, max_boots, crash, verify_every) {
            check(
                ended.capsule.as_ref(),
                ended.journal.events(),
                !crash && !ended.errors.is_empty(),
                number,
            )?;
        }
        let Ended {
            stop,
            mut errors,
            emergency,
            journal,
            capsule,
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
                machine::forget(&persistence, journal.dir())?;
                superseded = true;
            }
            kept = Some(journal);
            resume = capsule;
            simulator.reboot();
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
        simulator.reboot();
    }
    user.borrow_mut().flush().map_err(SimError::FlushLog)?;
    if outcome.is_none() {
        machine::save(&persistence, &lock(&persistence)?, &simulator.persisted())?;
    }
    if let Some(destination) = dump_to {
        dump(memory.as_ref(), &destination)?;
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

/// Closes a journal on an error path, where the error being returned is the one that matters.
fn release(journal: Option<Journal>) {
    if let Some(journal) = journal {
        drop(journal.close());
    }
}

fn publish(capsule: &ResumeCapsule, dir: &mut StateDir, boot: u32) -> Result<(), SimError> {
    capsule
        .publish(dir)
        .map_err(|source| SimError::Publish { boot, source })
}

#[expect(
    clippy::disallowed_methods,
    reason = "a simulation writes its in-memory state directory to disk on request"
)]
fn dump(memory: Option<&MemFs>, destination: &Path) -> Result<(), SimError> {
    match memory {
        Some(fs) => fs.dump_to(destination).map_err(SimError::Dump),
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
