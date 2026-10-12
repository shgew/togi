use std::cell::RefCell;
use std::io;
use std::num::NonZeroU32;
use std::path::Path;
use std::rc::Rc;

use togi_journal::{
    Event, FileError, Journal, LockError, LookupError, MemFs, ReadError, Recording, StateDir,
};
use togi_session::{Ended, PublicationPolicy, ResumeCapsule, RunInput, SessionError, prepare};

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
    let publication = match persistence {
        Persistence::Memory(_) => PublicationPolicy::OnFinish,
        Persistence::Files(_) => PublicationPolicy::Live,
    };
    let user: SharedLog = Rc::new(RefCell::new(UserLog::new(log)));
    let slot: Slot = Rc::new(RefCell::new(observer));
    let mut resume: Option<ResumeCapsule> = None;
    let mut outcome = None;
    let mut superseded = false;
    for number in 1..=max_boots.get() {
        let mut machine = simulator.seams();
        let host = |source| SimError::Host {
            boot: number,
            source,
        };
        let boot = machine.host.boot_id().map_err(host)?;
        let context = machine.host.bios_context().map_err(host)?;
        let prepared = prepare(
            lock(&persistence)?,
            &build,
            boot.clone(),
            simulator.now(),
            Some(&context),
            defects,
        )
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
        let mut ended = togi_session::run(RunInput {
            config: config.clone(),
            source: source.clone(),
            build: build.clone(),
            boot,
            journal,
            machine,
            policy,
            publication,
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
        if let Some(panic) = ended.panic.take() {
            panic.resume();
        }
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
            mut journal,
            capsule,
            panic: _,
        } = ended;
        errors.extend(user.borrow_mut().take_failure().map(SessionError::Log));
        if crash
            && publication == PublicationPolicy::OnFinish
            && let Some(capsule) = &capsule
        {
            capsule
                .publish(&mut journal)
                .map_err(|source| SimError::Publish {
                    boot: number,
                    source,
                })?;
        }
        close(journal, number)?;
        if appended && !superseded {
            machine::forget(&persistence, &lock(&persistence)?)?;
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
        let failures: Vec<SessionError> = errors
            .into_iter()
            .filter(|error| !error.is_crash())
            .collect();
        if !failures.is_empty() {
            return Err(SimError::Failed {
                boot: number,
                errors: failures,
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

fn close(journal: Journal, boot: u32) -> Result<(), SimError> {
    journal
        .close()
        .map(drop)
        .map_err(|source| SimError::Close { boot, source })
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
