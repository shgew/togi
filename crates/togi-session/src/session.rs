//! The tuning session: what a run is given, what it keeps while it runs, and how each of its steps ends.
//!
//! A run opens a [`Session`] from its [`RunInput`], then takes the startup steps in order, each returning whether
//! the run goes on: [`Session::start`], [`Session::preflight`], [`Session::reconcile`] and [`Session::begin`]. The
//! tuning loop runs [`Session::check_dead_end`] and [`Session::execute`] on every action, and every exit, whatever
//! ended the run, goes through [`Session::close`], which hands the journal back.

use std::io::{self, Write};
use std::num::NonZeroU32;
use std::time::Duration;

use jiff::tz::TimeZone;
use togi_carry::PendingCarry;
use togi_config::Config;
use togi_defect::FindingView;
use togi_journal::{
    Build, ConfigLoaded, DeadEnd, DeadEndAction, DeadEndCondition, Journal, Mce, Operation,
    Payload, Seq, SessionStart, SessionWarning, Shutdown, ShutdownReason, TrialId, Verdict,
};
use togi_machine::{
    BootId, CancellationToken, CoreInfo, CoreSet, Error, Host, Kernel, KernelCursor, Machine,
    Partial, Smu, SmuValidated, Stopped, Trials,
};
use togi_render::Renderer;
use togi_tuningboot::Bootloader;

use crate::appender::{Appender, Unresumed};
use crate::boot::{self, BootHandoff};
use crate::emergency::{self, SweepCleared};
use crate::error::{Flow, SessionError, Step};
use crate::evidence::{self, Placement, Window};
use crate::recovery::Cited;
use crate::write::Hardware;

/// When a run that met no dead end stops on its own.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StopPolicy {
    /// The clean cycles to run after phase 2 concluded.
    pub cycles: Option<NonZeroU32>,
    /// Stop at the next round or cycle start once the BIOS profile is confirmed.
    pub first_result: bool,
}

/// Where the configuration came from, as `config.loaded` records it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigSource {
    pub path: String,
    /// Whether a file was read; otherwise the defaults are in force.
    pub file: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Asks the person at the terminal about a defect finding.
pub trait Prompt {
    /// Whether to reset the cores the finding affected.
    fn reset_cores(&mut self, finding: &FindingView<'_>) -> Result<bool, PromptError>;
}

pub struct RunInput {
    pub config: Config,
    pub source: ConfigSource,
    /// This binary's build, which the journal's must match to be appended to.
    pub build: Build,
    pub boot: BootId,
    pub journal: Journal,
    pub machine: Machine,
    pub policy: StopPolicy,
    /// Set only in the tuning boot, where a dead end hands the next boot back to the normal system.
    pub bootloader: Option<Box<dyn Bootloader>>,
    /// `None` when no terminal can answer.
    pub prompt: Option<Box<dyn Prompt>>,
    /// The defects this build knows; [`togi_defect::KNOWN_DEFECTS`] outside tests.
    pub defects: &'static [togi_defect::Entry],
    /// What a transition carries into a new session.
    pub carry: Option<PendingCarry>,
    /// Receives each appended event's line.
    pub log: Option<Box<dyn Write>>,
    pub renderer: Renderer,
    pub zone: TimeZone,
    pub cancel: CancellationToken,
    /// Where the journal-failure emergency reports what it did.
    pub report: Box<dyn Write>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    Signal,
    DeadEnd,
    Cycles,
    FirstResult,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stop {
    pub reason: StopReason,
    /// The dead end and the events it cites, for [`StopReason::DeadEnd`].
    pub dead_end: Option<(DeadEnd, Vec<Seq>)>,
    /// The dead end asks the caller to reboot into the normal system.
    pub reboot: bool,
}

impl Stop {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the run steps after startup stop through it")
    )]
    const fn plain(reason: StopReason) -> Self {
        Self {
            reason,
            dead_end: None,
            reboot: false,
        }
    }
}

/// Whether the run goes on after a step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Control {
    Continue,
    Stop(Stop),
}

pub type Outcome = Result<Control, SessionError>;

/// How a run ended: the stop it reached, every error on the way out, and the journal handed back.
pub struct Ended {
    pub stop: Option<Stop>,
    /// The error that ended the run first, then the errors of its cleanup.
    pub errors: Vec<SessionError>,
    pub emergency: Option<emergency::Outcome>,
    pub journal: Journal,
}

/// What a kernel log read at a boundary leaves for the event that closes the boundary.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Boundary {
    pub cursor: Option<KernelCursor>,
    pub error: Option<String>,
}

/// The trial whose window a boundary's machine checks may fall in.
#[derive(Clone, Copy, Debug)]
pub struct TrialWindow<'a> {
    pub trial: &'a TrialId,
    pub since: Duration,
}

/// How this boot relates to the journal's history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Visit {
    Other,
    /// The journal already holds this boot; `reconciled` is whether the offsets an earlier run of it left have been
    /// read back (true when it left none).
    Same {
        reconciled: bool,
    },
}

/// What took the machine out of reach.
#[expect(dead_code, reason = "the run steps after startup set it")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    /// The workload's containment could not be confirmed: no SMU access, no restoration.
    Containment,
    /// The machine went down under the run.
    Crashed,
}

#[expect(
    dead_code,
    reason = "the run steps after startup read the machine and run state"
)]
pub struct Session {
    pub(crate) appender: Appender,
    pub(crate) smu: Box<dyn Smu>,
    pub(crate) host: Box<dyn Host>,
    pub(crate) trials: Box<dyn Trials>,
    pub(crate) kernel: Box<dyn Kernel>,
    pub(crate) boot: BootId,
    pub(crate) build: Build,
    pub(crate) config: Config,
    pub(crate) source: ConfigSource,
    pub(crate) policy: StopPolicy,
    pub(crate) prompt: Option<Box<dyn Prompt>>,
    pub(crate) carry: Option<PendingCarry>,
    pub(crate) cancel: CancellationToken,
    pub(crate) report: Box<dyn Write>,
    /// Sorted by core id.
    pub(crate) topology: Vec<CoreInfo>,
    pub(crate) hardware: Hardware,
    /// Proof that no workload runs; `None` before the sweep and while a trial runs or after containment failed.
    pub(crate) stopped: Option<Stopped>,
    pub(crate) sweep: Option<SweepCleared>,
    pub(crate) validated: Option<SmuValidated>,
    pub(crate) visit: Visit,
    pub(crate) recovery_cancelled: bool,
    /// Why the machine may no longer be touched, once it may not.
    pub(crate) fault: Option<Fault>,
    /// The `shutdown` the cleanup records once the machine is restored.
    pub(crate) shutdown: Option<Shutdown>,
}

impl Session {
    /// Takes over the journal: refuses one this build may not append to, reads the pending tuning-boot reason, folds
    /// the journal and reads the topology. Nothing is appended.
    pub fn open(input: RunInput) -> Result<Self, Box<Ended>> {
        let RunInput {
            config,
            source,
            build,
            boot,
            journal,
            machine,
            policy,
            mut bootloader,
            prompt,
            defects,
            carry,
            log,
            renderer,
            zone,
            cancel,
            report,
        } = input;
        let refused = |journal, error: SessionError| {
            Box::new(Ended {
                stop: None,
                errors: vec![error],
                emergency: None,
                journal,
            })
        };
        if let Some(refusal) = refusal(&build, &journal) {
            let mut errors = vec![SessionError::Compatibility(refusal)];
            if let Some(bootloader) = bootloader.as_deref_mut() {
                let left = boot::leave(bootloader, "journal incompatible");
                errors.extend(left.reason_error.map(SessionError::from));
                errors.extend(left.entry.err().map(SessionError::ClearSavedEntry));
            }
            return Err(Box::new(Ended {
                stop: None,
                errors,
                emergency: None,
                journal,
            }));
        }
        let handoff = match bootloader.map(BootHandoff::read).transpose() {
            Ok(handoff) => handoff,
            Err(error) => return Err(refused(journal, error.into())),
        };
        let Machine {
            clock,
            smu,
            host,
            trials,
            kernel,
        } = machine;
        let mut appender = match Appender::resume_with(journal, clock, renderer, zone, defects) {
            Ok(appender) => appender,
            Err(unresumed) => {
                let Unresumed { journal, error } = *unresumed;
                return Err(refused(journal, error.into()));
            }
        };
        if let Some(log) = log {
            appender = appender.with_log(log);
        }
        if let Some(handoff) = handoff {
            appender = appender.with_handoff(handoff);
        }
        let visit = if appender.recovery().boots().contains(&boot) {
            Visit::Same {
                reconciled: appender.recovery().baseline().is_none(),
            }
        } else {
            Visit::Other
        };
        Ok(Self {
            appender,
            smu,
            host,
            trials,
            kernel,
            boot,
            build,
            config,
            source,
            policy,
            prompt,
            carry,
            cancel,
            report,
            topology: Vec::new(),
            hardware: Hardware::new(CoreSet::default()),
            stopped: None,
            sweep: None,
            validated: None,
            visit,
            recovery_cancelled: false,
            fault: None,
            shutdown: None,
        })
    }

    pub const fn appender(&self) -> &Appender {
        &self.appender
    }

    pub const fn boot(&self) -> &BootId {
        &self.boot
    }

    pub fn cores(&self) -> CoreSet {
        self.hardware.cores()
    }

    pub const fn same_boot(&self) -> bool {
        matches!(self.visit, Visit::Same { .. })
    }

    const fn unreconciled(&self) -> bool {
        matches!(self.visit, Visit::Same { reconciled: false })
    }

    /// Finishes a dead end an earlier boot left pending, reads the topology and refuses configured cores it lacks;
    /// then, unless a dead end stays pending until this boot is reconciled, records the session's start, the
    /// startup kernel boundary and the configuration.
    pub fn start(&mut self) -> Outcome {
        if !self.same_boot()
            && let Some(stop) = self.resume_dead_end()?
        {
            return Ok(Control::Stop(stop));
        }
        let mut topology = self.host.topology()?;
        topology.sort_by_key(|info| info.core);
        let cores: CoreSet = topology.iter().map(|info| info.core).collect();
        if let Some((core, _)) = self
            .config
            .overrides()
            .find(|(core, _)| !cores.contains(*core))
        {
            return Err(SessionError::NoSuchCore { core });
        }
        self.topology = topology;
        self.hardware = Hardware::new(cores);
        if self.appender.recovery().pending_dead_end().is_some() {
            return Ok(Control::Continue);
        }
        if !self.appender.recovery().started() {
            let now = self.appender.clock().now();
            let session = self.appender.journal().state_dir().session_id(now)?;
            let start = SessionStart {
                build: self.build.clone(),
                session,
                cores: self.topology.clone(),
            };
            self.appender.append(Payload::SessionStart(start), &[])?;
        }
        let window = self.appender.recovery().open_trial().and_then(|open| {
            (open.boot == self.boot).then(|| (open.intent.trial.clone(), open.window_start))
        });
        let window = window.as_ref().map(|(trial, since)| TrialWindow {
            trial,
            since: since.unwrap_or_default(),
        });
        let boundary = match self.kernel_boundary(window, true, &[]) {
            Ok(boundary) => boundary,
            Err(flow) => return self.settle(Err(flow)),
        };
        let loaded = ConfigLoaded {
            build: self.build.clone(),
            config: self.config.clone(),
            path: self.source.path.clone(),
            file: self.source.file,
            kernel_cursor: boundary.cursor,
            kernel_error: boundary.error,
        };
        self.appender.append(Payload::ConfigLoaded(loaded), &[])?;
        self.warn_watchdog()?;
        Ok(Control::Continue)
    }

    fn warn_watchdog(&mut self) -> Result<(), SessionError> {
        if self.appender.handoff().is_some() {
            return Ok(());
        }
        let check = self.host.watchdog();
        if check.ok {
            return Ok(());
        }
        let warning = SessionWarning {
            operation: "check hardware watchdog".into(),
            trial: None,
            error: format!(
                "no active hardware watchdog ({}), so a freeze needs a manual reset; start sessions from the tuning \
                 boot, especially after a breaking update",
                check.detail
            ),
        };
        self.appender
            .append(Payload::SessionWarning(warning), &[])?;
        Ok(())
    }

    /// Reads the kernel log from the cursor recorded for this boot and records each new machine check: inside
    /// `window` it is the trial's; otherwise between trials, or, for an uncorrected one found while `recovering`
    /// with crashed boots to explain, from this boot for a crash to claim.
    pub(crate) fn kernel_boundary(
        &mut self,
        window: Option<TrialWindow<'_>>,
        recovering: bool,
        causes: &[Seq],
    ) -> Step<Boundary> {
        let mut cursor = self.appender.recovery().kernel_cursor(&self.boot).cloned();
        let (mces, error) = match self.kernel.read_mces(&self.boot, cursor.as_ref()) {
            Ok(read) => {
                cursor = Some(read.cursor);
                (read.mces, None)
            }
            Err(Partial {
                error: Error::Crashed,
                ..
            }) => return Err(SessionError::Machine(Error::Crashed).into()),
            Err(Partial {
                value,
                error: Error::CursorMissing,
            }) if cursor.is_some() => {
                let mut mces = value;
                match self.kernel.read_mces(&self.boot, None) {
                    Ok(fresh) => {
                        mces.extend(fresh.mces);
                        cursor = Some(fresh.cursor);
                        (mces, Some(Error::CursorMissing.to_string()))
                    }
                    Err(Partial {
                        error: Error::Crashed,
                        ..
                    }) => return Err(SessionError::Machine(Error::Crashed).into()),
                    Err(Partial { value, error }) => {
                        mces.extend(value);
                        (mces, Some(format!("{}; {error}", Error::CursorMissing)))
                    }
                }
            }
            Err(Partial { value, error }) => (value, Some(error.to_string())),
        };
        let until = self.appender.clock().monotonic();
        let crashed_boots = self
            .appender
            .recovery()
            .crashed_boots(&self.boot)
            .next()
            .is_some();
        for mce in mces {
            if self.appender.recovery().has_recorded(&self.boot, &mce) {
                continue;
            }
            let window_of = window.map(|window| Window {
                since: window.since,
                until,
            });
            let placement = evidence::place_kernel_mce(
                mce.monotonic,
                mce.corrected,
                window_of,
                recovering,
                crashed_boots,
            );
            let payload = Mce {
                cpu: mce.cpu,
                core: mce.core,
                bank: mce.bank,
                bank_type: mce.bank_type,
                corrected: mce.corrected,
                monotonic_ns: u64::try_from(mce.monotonic.as_nanos()).ok(),
                from_boot: (placement == Placement::FromBoot).then(|| self.boot.clone()),
                trial: window
                    .filter(|_| placement == Placement::Trial)
                    .map(|window| window.trial.clone()),
                between_trials: placement == Placement::BetweenTrials,
                lines: mce.lines,
            };
            self.appender.append(Payload::Mce(payload), causes)?;
        }
        Ok(Boundary {
            cursor,
            error: error.map(|error| format!("kernel log unreadable: {error}")),
        })
    }

    /// Ends the run with `reason`; the cleanup records the `shutdown` once the machine is restored.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the run steps after startup stop through it")
    )]
    pub(crate) fn shut_down(&mut self, reason: StopReason) -> Control {
        self.record_shutdown(reason);
        Control::Stop(Stop::plain(reason))
    }

    fn record_shutdown(&mut self, reason: StopReason) {
        let (recorded, cycles) = match reason {
            StopReason::Signal => (ShutdownReason::Signal, None),
            StopReason::DeadEnd => (ShutdownReason::DeadEnd, None),
            StopReason::Cycles => (ShutdownReason::Cycles, self.policy.cycles),
            StopReason::FirstResult => (ShutdownReason::FirstResult, None),
        };
        self.shutdown = Some(Shutdown {
            reason: recorded,
            kernel_cursor: None,
            kernel_error: None,
            cycles,
        });
    }

    /// The inconclusive trials in a row of one backend after which its retries begin.
    pub(crate) fn inconclusive_threshold(&self) -> NonZeroU32 {
        NonZeroU32::new(self.config.dead_ends().inconclusive_in_a_row().get())
            .unwrap_or(NonZeroU32::MIN)
    }

    /// Turns the end of a step into whether the run goes on: dead-end evidence becomes the dead end it supports.
    pub(crate) fn settle(&mut self, step: Step<Control>) -> Outcome {
        match step {
            Ok(control) => Ok(control),
            Err(Flow::Fatal(error)) => Err(error),
            Err(Flow::DeadEnd(condition)) => match self.check_dead_end()? {
                Some(stop) => Ok(Control::Stop(stop)),
                None => Err(SessionError::DeadEndUnrecorded(condition)),
            },
        }
    }

    /// The dead end the recorded evidence supports, strongest first.
    pub fn check_dead_end(&mut self) -> Result<Option<Stop>, SessionError> {
        let recovery = self.appender.recovery();
        let cited = |condition, cited: Option<&Cited>| {
            cited.map(|cited| (condition, cited.detail.clone(), vec![cited.seq]))
        };
        let inconclusive = self.inconclusive_threshold();
        let stray_limit = self.config.dead_ends().stray_crashes_in_a_row().get() as usize;
        let found = cited(DeadEndCondition::ThermalTrip, recovery.thermal_evidence())
            .or_else(|| cited(DeadEndCondition::NoEvidence, recovery.missing_evidence()))
            .or_else(|| cited(DeadEndCondition::Smu, recovery.smu_evidence()))
            .or_else(|| cited(DeadEndCondition::Containment, recovery.containment_evidence()))
            .or_else(|| {
                recovery
                    .exhausted_backend(inconclusive)
                    .map(|(backend, streak)| {
                        (
                            DeadEndCondition::NoEvidence,
                            format!(
                                "backend {backend} inconclusive {} times in a row after retries at 1, 5 and 30 min",
                                streak.len()
                            ),
                            streak.to_vec(),
                        )
                    })
            })
            .or_else(|| {
                let stray = recovery.stray();
                (stray.len() >= stray_limit).then(|| {
                    (
                        DeadEndCondition::BootLoop,
                        format!(
                            "{} crashes in a row before the profile was applied",
                            stray.len()
                        ),
                        stray.to_vec(),
                    )
                })
            });
        let Some((condition, detail, causes)) = found else {
            return Ok(None);
        };
        let dead_end = DeadEnd {
            condition,
            core: None,
            detail,
            action: None,
        };
        self.dead_end(dead_end, &causes).map(Some)
    }

    /// Records a dead end and leaves the tuning boot for it. On a boot with unreconciled offsets and a dead end
    /// already pending, nothing is recorded: the pending one is finished after reconciliation.
    pub(crate) fn dead_end(
        &mut self,
        mut dead_end: DeadEnd,
        causes: &[Seq],
    ) -> Result<Stop, SessionError> {
        dead_end.action = Some(match self.appender.handoff() {
            None => DeadEndAction::Exit,
            Some(_) if dead_end.condition == DeadEndCondition::BootLoop => {
                DeadEndAction::ClearSavedEntryAndReboot
            }
            Some(_) => DeadEndAction::ClearSavedEntry,
        });
        if self.unreconciled() && self.appender.recovery().pending_dead_end().is_some() {
            return Ok(dead_end_stop(dead_end, causes.to_vec(), false));
        }
        let seq = self
            .appender
            .append(Payload::DeadEnd(dead_end.clone()), causes)?;
        self.finish_dead_end(seq, dead_end, causes.to_vec(), true)
    }

    /// Finishes the dead end the journal holds pending, if any.
    pub(crate) fn resume_dead_end(&mut self) -> Result<Option<Stop>, SessionError> {
        let Some(pending) = self.appender.recovery().pending_dead_end() else {
            return Ok(None);
        };
        let (seq, dead_end, clear) = (
            pending.seq,
            pending.dead_end.clone(),
            pending.needs_saved_entry(),
        );
        let causes = self
            .appender
            .journal()
            .event(seq)
            .map(|event| event.cause.clone())
            .unwrap_or_default();
        self.finish_dead_end(seq, dead_end, causes, clear).map(Some)
    }

    fn finish_dead_end(
        &mut self,
        seq: Seq,
        dead_end: DeadEnd,
        causes: Vec<Seq>,
        clear: bool,
    ) -> Result<Stop, SessionError> {
        if self.unreconciled() {
            return Ok(dead_end_stop(dead_end, causes, false));
        }
        let action = dead_end.action.unwrap_or(DeadEndAction::Exit);
        let cleared = match action {
            DeadEndAction::Exit => true,
            DeadEndAction::ClearSavedEntry | DeadEndAction::ClearSavedEntryAndReboot if clear => {
                self.appender.import_boot_reason()?;
                let reason = format!("dead end {}: {}", dead_end.condition, dead_end.detail);
                match self
                    .appender
                    .handoff()
                    .map(|handoff| handoff.leave(&reason))
                {
                    None => false,
                    Some(left) => {
                        self.appender
                            .append(Payload::BootSavedEntry(left.payload()), &[seq])?;
                        left.entry.is_ok()
                    }
                }
            }
            DeadEndAction::ClearSavedEntry | DeadEndAction::ClearSavedEntryAndReboot => self
                .appender
                .journal()
                .events()
                .iter()
                .filter(|event| event.cause.contains(&seq))
                .filter_map(|event| match &event.payload {
                    Payload::BootSavedEntry(entry) => Some(entry.error.is_none()),
                    _ => None,
                })
                .next_back()
                .unwrap_or(false),
        };
        let reboot = action == DeadEndAction::ClearSavedEntryAndReboot && cleared;
        self.record_shutdown(StopReason::DeadEnd);
        Ok(dead_end_stop(dead_end, causes, reboot))
    }
}

fn dead_end_stop(dead_end: DeadEnd, causes: Vec<Seq>, reboot: bool) -> Stop {
    Stop {
        reason: StopReason::DeadEnd,
        dead_end: Some((dead_end, causes)),
        reboot,
    }
}

/// Why this build may not append to the journal, judged by the build of its `session.start`.
fn refusal(build: &Build, journal: &Journal) -> Option<togi_journal::Refusal> {
    let recorded = journal
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            Payload::SessionStart(start) => Some(&start.build),
            _ => None,
        })?;
    match build.verdict(recorded, Operation::Append) {
        Verdict::Refuse(refusal) => Some(refusal),
        Verdict::Use | Verdict::Archive => None,
    }
}

#[cfg(test)]
mod tests;
