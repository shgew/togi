//! `togi day`: stock-only application and durable observation of normal boots.

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use clap::Args as ClapArgs;
use togi_hardware::day::Observer;
use togi_journal::{
    Build, DayBootStart, DayCheckpoint, DayCleanStop, DayIntervalCredit, DayIntervals,
    DayOffsetsChanged, DayUncleanEnd, Event, Journal, LoadMix, Payload, Seq, Stamp,
};
use togi_machine::{
    BootId, CancellationToken, Clock, CoreId, CoreInfo, ObservedOffset, PerCore, ResetReason,
    WatchdogState,
};

use crate::cli::Context;
use crate::exit::Exit;
use crate::report::{current_build, exit_with};

mod apply;
mod faults;
mod load_mix;
mod notify;
mod record;
pub mod socket;

use faults::Faults;
use load_mix::{SAMPLE, Sampler};

const CHECKPOINT: Duration = Duration::from_secs(600);

/// How often the kernel log is read for new faults. A fault is journaled with the offsets last read back, so
/// reading more often than that matters less than reading at all; half a minute keeps the journalctl process
/// off the machine's busy seconds.
const FAULT_POLL: Duration = Duration::from_secs(30);

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    crate::cli::examples(&[
        ("sudo togi day", "Apply stock and observe this boot"),
        ("togi events --kind day", "Read recorded day observations"),
        (
            "togi day answer <boot> frozen",
            "Answer a pending question without root",
        ),
        (
            "sudo togi day record",
            "Record a crash togi did not observe",
        ),
    ])
});

/// Apply stock, observe daily use, answer an unclean end, or record a crash togi missed
///
/// With no subcommand, requires an all-core CO-0 baseline and PID1's armed, counting watchdog,
/// then applies stock through durable intent, write and readback. It records active hours excluding
/// suspend and journals kernel faults. Stop signals zero and verify every core before the final
/// checkpoint and clean marker. It requires root and an existing compatible session journal.
/// The service holds the host and writer locks: stop it before tuning; status/watch stay lock-free.
///
/// Answer and notify use the credential-checked day socket without root or journal access.
/// Record requires root and hands the record to the live writer, opening the writer itself only
/// when no day service runs. Nonzero day offsets are not enabled.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// User-session commands use the day socket and never open the journal writer.
#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Answer an unclean end; a later answer replaces the previous answer.
    Answer {
        /// Full boot ID shown in the pending question.
        boot: String,
        /// What happened: frozen, graphics, power-off or unsure.
        #[arg(value_parser = parse_answer)]
        answer: togi_journal::DayAnswerKind,
    },
    /// Send pending questions and the morning evidence report to this graphical session.
    Notify,
    /// Reapply stock after system resume; used by the system sleep hook.
    #[command(hide = true)]
    Reapply,
    Record(record::Args),
}

fn parse_answer(value: &str) -> Result<togi_journal::DayAnswerKind, String> {
    togi_watch::day::ANSWERS
        .into_iter()
        .find(|kind| togi_watch::day::answer_arg(*kind) == value)
        .ok_or_else(|| format!("expected one of {}", togi_watch::day::answer_args()))
}

/// Runs daily observation until a stop signal or an observation/journal error.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    if let Some(command) = &args.command {
        return match command {
            Command::Answer { boot, answer } => answer_end(boot, *answer, ctx),
            Command::Notify => notify::run(ctx),
            Command::Record(record) => record::run(record, ctx),
            Command::Reapply => reapply(ctx),
        };
    }
    if let Err(error) = ctx.system.check_platform() {
        return exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error);
    }
    if !ctx.system.privileged() {
        return exit_with(ctx, "day", "daily application needs root", Exit::Error);
    }
    let build = current_build();
    if let Err(exit) = crate::day_destination::available(ctx, &build) {
        return exit;
    }
    let cancel = CancellationToken::new();
    let _listening = match ctx.system.listen(&cancel) {
        Ok(listening) => listening,
        Err(error) => return exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error),
    };
    let lock = match ctx.system.acquire_host_lock() {
        Ok(lock) => lock,
        Err(error @ togi_hostlock::Error::Busy { .. }) => {
            return exit_with(ctx, "day", error, Exit::Locked);
        }
        Err(error) => return exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error),
    };
    let boot = match ctx.system.boot_id() {
        Ok(boot) => boot,
        Err(error) => return exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error),
    };
    let mut journal = match crate::day_destination::open(ctx, boot, &build) {
        Ok(journal) => journal,
        Err(exit) => return exit,
    };
    let mut observer = match ctx.system.day_observer(&lock) {
        Ok(observer) => observer,
        Err(error) => return exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error),
    };
    let socket = match ctx.system.day_socket() {
        Ok(socket) => socket,
        Err(error) => return exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error),
    };
    let mut clock = crate::system::SystemClock(ctx.system);
    let result = serve_with_socket(
        &mut journal,
        &mut *observer,
        &mut clock,
        &cancel,
        &build,
        socket.as_ref(),
    );
    // Close before dropping hardware and its borrowed exclusion lock.
    let closed = journal.close();
    if let Err(error) = result {
        return exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error);
    }
    match closed {
        Ok(_dir) => Exit::Ok,
        Err(error) => {
            if let Err(cleanup) = apply::emergency(&mut *observer) {
                return exit_with(
                    ctx,
                    "day emergency zero",
                    togi_render::error_chain(&cleanup),
                    Exit::Error,
                );
            }
            exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error)
        }
    }
}

fn answer_end(boot: &str, answer: togi_journal::DayAnswerKind, ctx: &mut Context<'_>) -> Exit {
    match ctx.system.day_request(&socket::Request::Answer {
        boot: boot.to_owned().into(),
        answer,
    }) {
        Ok(socket::Reply::Answered) => {
            crate::report::emit(ctx, format_args!("answer recorded for boot {boot}"))
                .map_or_else(|exit| exit, |()| Exit::Ok)
        }
        Ok(
            socket::Reply::Questions { .. }
            | socket::Reply::Recorded { .. }
            | socket::Reply::Reapplied
            | socket::Reply::Refused { .. },
        ) => exit_with(
            ctx,
            "day answer",
            "unexpected day service reply",
            Exit::Error,
        ),
        Err(error) => exit_with(
            ctx,
            "day answer",
            togi_render::error_chain(&error),
            Exit::Error,
        ),
    }
}

fn reapply(ctx: &mut Context<'_>) -> Exit {
    match ctx.system.day_request(&socket::Request::Reapply) {
        Ok(socket::Reply::Reapplied) => Exit::Ok,
        Ok(
            socket::Reply::Questions { .. }
            | socket::Reply::Answered
            | socket::Reply::Recorded { .. }
            | socket::Reply::Refused { .. },
        ) => exit_with(
            ctx,
            "day reapply",
            "unexpected day service reply",
            Exit::Error,
        ),
        Err(error) => exit_with(
            ctx,
            "day reapply",
            togi_render::error_chain(&error),
            Exit::Error,
        ),
    }
}

/// An observation failure never fabricates a clean-stop marker.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("read daily observation")]
    Observe(#[from] togi_machine::Error),
    #[error("append daily observation")]
    Append(#[from] togi_journal::AppendError),
    #[error("the monotonic clock moved backwards")]
    ClockReversed,
    #[error("daily observation duration overflowed")]
    DurationOverflow,
    #[error("the session journal records no core topology to group the sampled CPUs by")]
    Topology,
    #[error("serve day questions")]
    Socket(#[from] socket::Error),
    #[error("daily application requires every recorded core to read CO 0")]
    Baseline,
    #[error("daily application requires PID1's hardware watchdog to be armed and counting")]
    Watchdog,
    #[error("nonzero daily application is blocked by an unresolved too-aggressive defect")]
    Defect,
    #[error("day application identity overflowed")]
    ApplicationOverflow,
    #[error("invalid day profile")]
    Profile(#[from] togi_machine::CcdProfileError),
    #[error("fold daily policy")]
    PolicyFold(#[from] togi_tuner::policy::FoldError),
    #[error("query daily policy")]
    PolicyQuery(#[from] togi_tuner::policy::NotReady),
    #[error("apply stock day profile")]
    Apply(#[from] togi_session::DayWriteError),
    #[error("this observation has no applying capability")]
    NotApplying,
    #[error("emergency zero failed after daily service error: {original}")]
    Emergency {
        original: Box<Error>,
        #[source]
        cleanup: togi_machine::Error,
    },
}

fn append(
    journal: &mut Journal,
    clock: &dyn Clock,
    payload: impl Into<Payload>,
) -> Result<(), Error> {
    journal.append(
        Stamp {
            time: clock.now(),
            mono: Some(clock.monotonic()?),
        },
        payload.into(),
        &[],
    )?;
    Ok(())
}

/// Observes through injected read-only hardware and active-time clock seams.
///
/// # Errors
/// Returns the first read, clock or durable append failure; no clean marker follows it.
pub fn observe(
    journal: &mut Journal,
    observer: &mut dyn Observer,
    clock: &mut dyn Clock,
    cancel: &CancellationToken,
    build: &Build,
) -> Result<(), Error> {
    observe_with_socket(journal, observer, clock, cancel, build, None, None)
}

/// Applies stock and observes through the same seams used by the running service.
///
/// # Errors
/// Refuses an unverified stock baseline or watchdog. A later error takes the journal-independent
/// zero-only emergency path and never fabricates a clean marker.
pub fn serve(
    journal: &mut Journal,
    observer: &mut dyn Observer,
    clock: &mut dyn Clock,
    cancel: &CancellationToken,
    build: &Build,
) -> Result<(), Error> {
    serve_with_socket(journal, observer, clock, cancel, build, None)
}

fn serve_with_socket(
    journal: &mut Journal,
    observer: &mut dyn Observer,
    clock: &mut dyn Clock,
    cancel: &CancellationToken,
    build: &Build,
    socket: Option<&socket::Server>,
) -> Result<(), Error> {
    let mut live = apply::Live::preflight(journal, observer)?;
    let result = observe_with_socket(
        journal,
        observer,
        clock,
        cancel,
        build,
        socket,
        Some(&mut live),
    );
    if let Err(original) = result {
        if let Err(cleanup) = apply::emergency(observer) {
            return Err(Error::Emergency {
                original: Box::new(original),
                cleanup,
            });
        }
        return Err(original);
    }
    Ok(())
}

fn observe_with_socket(
    journal: &mut Journal,
    observer: &mut dyn Observer,
    clock: &mut dyn Clock,
    cancel: &CancellationToken,
    build: &Build,
    socket: Option<&socket::Server>,
    mut live: Option<&mut apply::Live>,
) -> Result<(), Error> {
    let ledger = Ledger::of(journal.events());
    for (boot, previous) in &ledger.boots {
        if boot != journal.boot() && !previous.clean && !previous.unclean {
            // Missing or unreadable successor evidence is explicitly unknown, never CPU evidence.
            let reset_reason = match observer.reset_reason_after(boot) {
                Ok(reason) => reason,
                Err(_unavailable) => ResetReason {
                    kind: None,
                    raw: Vec::new(),
                    supported: false,
                },
            };
            append(
                journal,
                clock,
                DayUncleanEnd {
                    boot: boot.clone(),
                    last_checkpoint: previous.checkpoint,
                    offsets: previous.offsets,
                    reset_reason,
                    evidence: observer.end_evidence(boot),
                },
            )?;
        }
    }
    let accumulated = ledger
        .boots
        .get(journal.boot())
        .map_or(Duration::ZERO, |boot| Duration::from_secs(boot.seconds));
    let started = clock.monotonic()?;
    let bios = observer.bios_context()?;
    let mut offsets = observer.offsets()?;
    let mut watchdog = observer.watchdog();
    append(
        journal,
        clock,
        DayBootStart {
            build: build.clone(),
            bios,
            offsets,
            watchdog: Some(watchdog.clone()),
        },
    )?;
    if let Some(live) = live.as_deref_mut() {
        live.start(
            journal,
            observer,
            clock,
            ledger.boots.contains_key(journal.boot()),
        )?;
    }
    let mut sampler = Sampler::new(
        session_cores(journal.events())?,
        ledger
            .boots
            .get(journal.boot())
            .map_or_else(LoadMix::default, |boot| boot.load_mix),
    );
    sampler.record(started, observer.cpu_times()?)?;
    let mut faults = Faults::of(journal.events(), journal.boot());
    faults.poll(journal, observer, clock, offsets)?;
    let mut checkpointed = started;
    let mut sampled = started;
    let mut polled = started;
    loop {
        if let Some(socket) = socket {
            socket.poll(journal, clock, &mut |journal, clock| {
                live.as_deref_mut()
                    .ok_or(Error::NotApplying)?
                    .reapply(journal, observer, clock)
            })?;
        }
        let wait = if socket.is_some() {
            Duration::from_secs(1)
        } else {
            SAMPLE
        };
        match clock.sleep(cancel, wait) {
            Ok(()) | Err(togi_machine::Error::Cancelled) => {}
            Err(error) => return Err(error.into()),
        }
        let now = clock.monotonic()?;
        let stopping = cancel.is_cancelled();
        let due = now.checked_sub(checkpointed).ok_or(Error::ClockReversed)? >= CHECKPOINT;
        let sample = now.checked_sub(sampled).ok_or(Error::ClockReversed)? >= SAMPLE;
        // The first scheduled sample after waking checks again too: active state alone can survive S3
        // while the timer loses its MMIO decoding. Changes get a checkpoint immediately.
        let watchdog_changed = if stopping || sample || due {
            let checked = observer.watchdog();
            let changed = checked != watchdog;
            watchdog = checked;
            changed
        } else {
            false
        };
        if stopping || sample {
            sampler.record(now, observer.cpu_times()?)?;
            sampled = now;
        }
        if stopping || now.checked_sub(polled).ok_or(Error::ClockReversed)? >= FAULT_POLL {
            faults.poll(journal, observer, clock, offsets)?;
            polled = now;
        }
        if !stopping && !due && !watchdog_changed {
            continue;
        }
        checkpointed = now;
        let elapsed = now.checked_sub(started).ok_or(Error::ClockReversed)?;
        let applied = accumulated
            .checked_add(elapsed)
            .ok_or(Error::DurationOverflow)?;
        let readback = if stopping && let Some(live) = live.as_deref_mut() {
            live.zero(journal, observer, clock)?
        } else {
            observer.offsets()?
        };
        if readback != offsets {
            append(journal, clock, DayOffsetsChanged { offsets: readback })?;
            offsets = readback;
        }
        append(
            journal,
            clock,
            DayCheckpoint {
                applied_secs: applied.as_secs(),
                load_mix: sampler.mix(),
                watchdog: Some(watchdog.clone()),
            },
        )?;
        if stopping {
            append(journal, clock, DayCleanStop {})?;
            return Ok(());
        }
    }
}

/// The cores the session journal recorded, which group the sampled CPUs.
fn session_cores(events: &[Event]) -> Result<&[CoreInfo], Error> {
    events
        .iter()
        .find_map(|event| {
            if let Payload::SessionStart(start) = &event.payload {
                Some(start.cores.as_slice())
            } else {
                None
            }
        })
        .ok_or(Error::Topology)
}

pub(crate) struct Boot {
    pub offsets: PerCore<ObservedOffset>,
    pub checkpoint: Option<Seq>,
    pub seconds: u64,
    pub load_mix: LoadMix,
    pub clean: bool,
    pub unclean: bool,
}

pub(crate) type Profile = Vec<(CoreId, i64)>;

#[derive(Default)]
pub(crate) struct Ledger {
    pub boots: BTreeMap<BootId, Boot>,
    pub profiles: BTreeMap<Profile, u64>,
    pub last_unclean: Option<(BootId, ResetReason)>,
    /// The newest watchdog check; `None` when the newest observer start predates the check.
    pub watchdog: Option<WatchdogState>,
}

impl Ledger {
    pub(crate) fn of(events: &[Event]) -> Self {
        let mut ledger = Self::default();
        let mut intervals = DayIntervals::default();
        for event in events {
            if let Some(DayIntervalCredit::Credited {
                offsets, seconds, ..
            }) = intervals.apply(event)
            {
                let profile = offsets
                    .iter()
                    .map(|(core, offset)| (core, offset.0))
                    .collect();
                let total = ledger.profiles.entry(profile).or_default();
                *total = checkpointed_seconds(*total, seconds);
            }
            if let Payload::DayBootStart(start) = &event.payload
                && let Some(observed) = intervals.get(&event.boot)
            {
                ledger.watchdog.clone_from(&start.watchdog);
                let previous = ledger.boots.remove(&event.boot);
                ledger.boots.insert(
                    event.boot.clone(),
                    Boot {
                        offsets: observed.offsets,
                        checkpoint: previous.as_ref().and_then(|boot| boot.checkpoint),
                        seconds: observed.seconds,
                        load_mix: previous
                            .as_ref()
                            .map_or_else(LoadMix::default, |boot| boot.load_mix),
                        clean: false,
                        unclean: false,
                    },
                );
            }
            if let Payload::DayOffsetsChanged(_) = &event.payload
                && let Some(observed) = intervals.get(&event.boot)
                && let Some(boot) = ledger.boots.get_mut(&event.boot)
            {
                boot.offsets = observed.offsets;
            }
            if let Payload::DayCheckpoint(checkpoint) = &event.payload {
                if let Some(state) = &checkpoint.watchdog {
                    ledger.watchdog = Some(state.clone());
                }
                if let Some(observed) = intervals.get(&event.boot)
                    && let Some(boot) = ledger.boots.get_mut(&event.boot)
                {
                    boot.seconds = observed.seconds;
                    boot.load_mix = checkpoint.load_mix;
                    boot.checkpoint = Some(event.seq);
                }
            }
            if let Payload::DayCleanStop(_) = &event.payload {
                if let Some(boot) = ledger.boots.get_mut(&event.boot) {
                    boot.clean = true;
                }
            }
            if let Payload::DayUncleanEnd(end) = &event.payload {
                if let Some(boot) = ledger.boots.get_mut(&end.boot) {
                    boot.unclean = true;
                }
                ledger.last_unclean = Some((end.boot.clone(), end.reset_reason.clone()));
            }
        }
        ledger
    }
}

/// Exposure summaries saturate at the representable lifetime; individual checkpoints stay exact.
fn checkpointed_seconds(total: u64, additional: u64) -> u64 {
    total.saturating_add(additional)
}

#[cfg(test)]
mod tests;
