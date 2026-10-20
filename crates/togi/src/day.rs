//! `togi day`: durable, read-only observation of normal boots.

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use clap::Args as ClapArgs;
use togi_hardware::day::Observer;
use togi_journal::{
    Build, DayBootStart, DayCheckpoint, DayCleanStop, DayOffsetsChanged, DayUncleanEnd, Event,
    Journal, Payload, Seq, Stamp,
};
use togi_machine::{
    BootId, CancellationToken, Clock, CoreId, ObservedOffset, PerCore, ResetReason,
};

use crate::cli::Context;
use crate::exit::Exit;
use crate::report::{current_build, exit_with};

const CHECKPOINT: Duration = Duration::from_secs(600);

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    crate::cli::examples(&[
        (
            "sudo togi day",
            "Observe this boot without applying offsets",
        ),
        ("togi events --kind day", "Read recorded day observations"),
    ])
});

/// Observe normal use without applying offsets or starting workloads
///
/// Reads every core at start and each ten-minute checkpoint, records active hours excluding
/// suspend, and writes a final checkpoint before a clean stop on SIGTERM, SIGINT or SIGHUP.
/// Requires root and an existing compatible session journal. Holds the same host and writer
/// locks as run/reset: stop the day service before tuning in this boot. Status/watch stay lock-free.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {}

/// Runs daily observation until a stop signal or an observation/journal error.
pub fn run(_args: &Args, ctx: &mut Context<'_>) -> Exit {
    if let Err(error) = ctx.system.check_platform() {
        return exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error);
    }
    if !ctx.system.privileged() {
        return exit_with(ctx, "day", "daily observation needs root", Exit::Error);
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
    let mut clock = crate::system::SystemClock(ctx.system);
    let result = observe(&mut journal, &mut *observer, &mut clock, &cancel, &build);
    // Close before dropping the hardware capability and its borrowed host lock.
    let closed = journal.close();
    if let Err(error) = result {
        return exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error);
    }
    match closed {
        Ok(_dir) => Exit::Ok,
        Err(error) => exit_with(ctx, "day", togi_render::error_chain(&error), Exit::Error),
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
    append(
        journal,
        clock,
        DayBootStart {
            build: build.clone(),
            bios,
            offsets,
        },
    )?;
    loop {
        match clock.sleep(cancel, CHECKPOINT) {
            Ok(()) | Err(togi_machine::Error::Cancelled) => {}
            Err(error) => return Err(error.into()),
        }
        let now = clock.monotonic()?;
        let elapsed = now.checked_sub(started).ok_or(Error::ClockReversed)?;
        let applied = accumulated
            .checked_add(elapsed)
            .ok_or(Error::DurationOverflow)?;
        let readback = observer.offsets()?;
        if readback != offsets {
            append(journal, clock, DayOffsetsChanged { offsets: readback })?;
            offsets = readback;
        }
        append(
            journal,
            clock,
            DayCheckpoint {
                applied_secs: applied.as_secs(),
            },
        )?;
        if cancel.is_cancelled() {
            append(journal, clock, DayCleanStop {})?;
            return Ok(());
        }
    }
}

pub(crate) struct Boot {
    pub offsets: PerCore<ObservedOffset>,
    pub checkpoint: Option<Seq>,
    pub seconds: u64,
    pub clean: bool,
    pub unclean: bool,
    changed: bool,
}

pub(crate) type Profile = Vec<(CoreId, i64)>;

#[derive(Default)]
pub(crate) struct Ledger {
    pub boots: BTreeMap<BootId, Boot>,
    pub profiles: BTreeMap<Profile, u64>,
    pub last_unclean: Option<(BootId, ResetReason)>,
}

impl Ledger {
    pub(crate) fn of(events: &[Event]) -> Self {
        let mut ledger = Self::default();
        for event in events {
            if let Payload::DayBootStart(start) = &event.payload {
                let previous = ledger.boots.remove(&event.boot);
                ledger.boots.insert(
                    event.boot.clone(),
                    Boot {
                        offsets: start.offsets,
                        checkpoint: previous.as_ref().and_then(|boot| boot.checkpoint),
                        seconds: previous.as_ref().map_or(0, |boot| boot.seconds),
                        clean: false,
                        unclean: false,
                        changed: false,
                    },
                );
            }
            if let Payload::DayOffsetsChanged(change) = &event.payload {
                if let Some(boot) = ledger.boots.get_mut(&event.boot) {
                    boot.offsets = change.offsets;
                    boot.changed = true;
                }
            }
            if let Payload::DayCheckpoint(checkpoint) = &event.payload {
                if let Some(boot) = ledger.boots.get_mut(&event.boot) {
                    if !boot.changed {
                        let profile = boot
                            .offsets
                            .iter()
                            .map(|(core, offset)| (core, offset.0))
                            .collect();
                        let total = ledger.profiles.entry(profile).or_default();
                        *total = checkpointed_seconds(
                            *total,
                            checkpoint.applied_secs.saturating_sub(boot.seconds),
                        );
                    }
                    boot.seconds = checkpoint.applied_secs;
                    boot.checkpoint = Some(event.seq);
                    boot.changed = false;
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
