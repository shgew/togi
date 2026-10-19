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
use togi_config::{Config, DeadEndThreshold};
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
use togi_tuner::Topology;
use togi_tuningboot::Bootloader;

use crate::appender::{AppendHook, Appender, Unresumed};
use crate::boot::{self, BootHandoff};
use crate::emergency::{self, SweepCleared};
use crate::error::{Flow, SessionError, Step};
use crate::evidence::{self, Placement, Window};
use crate::recover::KernelUnreadable;
use crate::recovery::Cited;
use crate::resume::ResumeCapsule;
use crate::write::{Hardware, Verified};

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

pub struct RunInput<'machine> {
    pub config: Config,
    pub source: ConfigSource,
    /// This binary's build, which the journal's must match to be appended to.
    pub build: Build,
    pub boot: BootId,
    pub journal: Journal,
    pub machine: Machine<'machine>,
    pub policy: StopPolicy,
    pub publication: crate::publication::PublicationPolicy,
    /// Set only in the tuning boot, where a dead end hands the next boot back to the normal system.
    pub bootloader: Option<Box<dyn Bootloader>>,
    /// `None` when no terminal can answer.
    pub prompt: Option<Box<dyn Prompt>>,
    /// The defects this build knows; [`togi_defect::KNOWN_DEFECTS`] outside tests.
    pub defects: &'static [togi_defect::Entry],
    /// What the previous run on this journal left; adopted only when it covers exactly the journal.
    pub resume: Option<ResumeCapsule>,
    /// What a transition carries into a new session.
    pub carry: Option<PendingCarry>,
    /// Receives each appended event's line.
    pub log: Option<Box<dyn Write>>,
    /// Called with every appended event once, in order, after it is durable and its log line is written.
    pub on_append: Option<AppendHook>,
    pub renderer: Renderer,
    pub zone: TimeZone,
    pub cancel: CancellationToken,
    /// Where the journal-failure emergency reports what it did.
    pub report: Box<dyn Write>,
}

/// Why the run ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Stop {
    Signal,
    Cycles,
    FirstResult,
    DeadEnd {
        dead_end: DeadEnd,
        /// The events the dead end cites.
        causes: Vec<Seq>,
        /// The dead end asks the caller to reboot into the normal system.
        reboot: bool,
    },
}

/// Whether the run goes on after a step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Control {
    Continue,
    Stop(Stop),
}

pub(crate) type Outcome = Result<Control, SessionError>;

/// How a run ended: the stop it reached, every error on the way out, and the journal handed back.
#[must_use = "an ended run carries the errors, emergency and panic to report and the journal to close"]
pub struct Ended {
    pub stop: Option<Stop>,
    /// The error that ended the run first, then the errors of its cleanup.
    pub errors: Vec<SessionError>,
    pub emergency: Option<emergency::Outcome>,
    pub journal: Journal,
    /// What the next run on this journal may adopt instead of folding it again: `None` when the run latched a
    /// journal failure, which leaves the journal's last state unproven. A crashed run keeps its capsule: the folds
    /// follow every append, whatever was published.
    pub capsule: Option<ResumeCapsule>,
    /// The panic that ended the run, once the cleanup after it finished: the caller closes what it holds and then
    /// resumes it, so the panic still propagates.
    pub panic: Option<Panic>,
}

/// The payload of a panic that unwound out of a run.
#[must_use = "a panic caught for the cleanup is resumed once the caller has put its own state away"]
pub struct Panic(Box<dyn std::any::Any + Send>);

impl Panic {
    pub(crate) fn new(payload: Box<dyn std::any::Any + Send>) -> Self {
        Self(payload)
    }

    /// The panic's message, when it carries a string.
    #[must_use]
    pub fn message(&self) -> &str {
        message_of(&*self.0)
    }

    /// Propagates the original panic.
    pub fn resume(self) -> ! {
        std::panic::resume_unwind(self.0)
    }
}

impl std::fmt::Debug for Panic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Panic").field(&self.message()).finish()
    }
}

pub(crate) fn message_of(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

impl Ended {
    /// Whether any error of the run is one the machine's death or reboot would explain.
    #[must_use]
    pub fn crashed(&self) -> bool {
        self.errors.iter().any(SessionError::is_crash)
    }
}

/// What a kernel log read at a boundary leaves for the event that closes the boundary.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Boundary {
    pub cursor: Option<KernelCursor>,
    pub error: Option<String>,
}

/// The trial whose window a boundary's machine checks may fall in.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TrialWindow<'a> {
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fault {
    /// The workload's containment could not be confirmed: no SMU access, no restoration.
    Containment,
    /// The machine went down under the run.
    Crashed,
}

/// What the run may do with the machine: each state holds only the proofs its hardware access needs.
#[derive(Debug)]
pub(crate) enum Lifecycle {
    /// No sweep succeeded yet: a stale scope may still hold a workload, so nothing writes the SMU.
    Unswept,
    /// The stale-scope sweep succeeded.
    Swept(Swept),
    /// The machine may no longer be touched.
    Faulted(Fault),
}

/// The proofs a run holds once its sweep succeeded.
#[derive(Debug)]
pub(crate) struct Swept {
    pub cleared: SweepCleared,
    /// Proof that no workload runs; `None` while a trial holds it.
    pub stopped: Option<Stopped>,
    /// The write transaction that last put the machine in the state the next trial starts on; `None` once a write
    /// of the run failed or before any succeeded.
    pub confirmed: Option<Verified>,
}

impl Lifecycle {
    pub(crate) const fn swept(cleared: SweepCleared, stopped: Stopped) -> Self {
        Self::Swept(Swept {
            cleared,
            stopped: Some(stopped),
            confirmed: None,
        })
    }

    pub(crate) const fn fault(&self) -> Option<Fault> {
        match self {
            Self::Faulted(fault) => Some(*fault),
            Self::Unswept | Self::Swept(_) => None,
        }
    }

    /// The proof that no workload runs, while the run holds it.
    pub(crate) const fn stopped(&self) -> Option<&Stopped> {
        match self {
            Self::Swept(Swept {
                stopped: Some(stopped),
                ..
            }) => Some(stopped),
            Self::Swept(_) | Self::Unswept | Self::Faulted(_) => None,
        }
    }

    /// Takes the machine out of reach for `fault`, dropping every proof; a fault already latched stays.
    pub(crate) fn latch(&mut self, fault: Fault) {
        if !matches!(self, Self::Faulted(_)) {
            *self = Self::Faulted(fault);
        }
    }

    /// The machine went down: that replaces whatever the run held or knew before.
    pub(crate) fn crash(&mut self) {
        *self = Self::Faulted(Fault::Crashed);
    }

    /// Hands back the proof that no workload runs, unless the machine is out of reach by now.
    pub(crate) fn hold(&mut self, stopped: Stopped) {
        if let Self::Swept(swept) = self {
            swept.stopped = Some(stopped);
        }
    }

    /// Forgets the proof that no workload runs, as a trial that holds it does.
    #[cfg(test)]
    pub(crate) fn lose_stopped(&mut self) {
        if let Self::Swept(swept) = self {
            swept.stopped = None;
        }
    }
}

pub(crate) struct Session<'machine> {
    pub(crate) appender: Appender<'machine>,
    pub(crate) smu: Box<dyn Smu + 'machine>,
    pub(crate) host: Box<dyn Host + 'machine>,
    pub(crate) trials: Box<dyn Trials + 'machine>,
    pub(crate) kernel: Box<dyn Kernel + 'machine>,
    pub(crate) boot: BootId,
    pub(crate) build: Build,
    pub(crate) config: Config,
    pub(crate) source: ConfigSource,
    pub(crate) policy: StopPolicy,
    pub(crate) prompt: Option<Box<dyn Prompt>>,
    pub(crate) carry: Option<PendingCarry>,
    pub(crate) cancel: CancellationToken,
    pub(crate) report: Box<dyn Write>,
    /// Sorted by core id, as [`Session::open`] read it.
    pub(crate) topology: Vec<CoreInfo>,
    pub(crate) hardware: Hardware,
    pub(crate) lifecycle: Lifecycle,
    /// Proof that the SMU interface is the one togi expects; preflight's, independent of the sweep.
    pub(crate) validated: Option<SmuValidated>,
    pub(crate) visit: Visit,
    pub(crate) recovery_cancelled: bool,
    pub(crate) unreadable: Option<KernelUnreadable>,
    /// What a join of a workload after a failed record or a panic reported when it could not confirm the stop; the
    /// close reports it after the error that ended the run.
    pub(crate) unconfirmed: Vec<togi_machine::Error>,
    /// The `shutdown` the cleanup records once the machine is restored.
    pub(crate) shutdown: Option<Shutdown>,
}

impl<'machine> Session<'machine> {
    /// Takes over the journal: refuses one this build may not append to, reads the topology and, for a journal with
    /// no session yet, refuses one the tuner cannot work on; then reads the pending tuning-boot reason and folds the
    /// journal. Nothing is appended.
    pub(crate) fn open(input: RunInput<'machine>) -> Result<Self, Box<Ended>> {
        let RunInput {
            config,
            source,
            build,
            boot,
            journal,
            machine,
            policy,
            publication,
            mut bootloader,
            prompt,
            defects,
            resume,
            carry,
            log,
            on_append,
            renderer,
            zone,
            cancel,
            report,
        } = input;
        let refused = |journal, errors: Vec<SessionError>| {
            Box::new(Ended {
                stop: None,
                errors,
                emergency: None,
                journal,
                capsule: None,
                panic: None,
            })
        };
        if let Some(refusal) = refusal(&build, &journal) {
            let errors = leaving(
                SessionError::Compatibility(refusal),
                &mut bootloader,
                "journal incompatible",
            );
            return Err(refused(journal, errors));
        }
        let Machine {
            clock,
            smu,
            mut host,
            trials,
            kernel,
        } = machine;
        let mut topology = match host.topology() {
            Ok(topology) => topology,
            Err(error) => return Err(refused(journal, vec![error.into()])),
        };
        topology.sort_by_key(|info| info.core);
        if journal.events().is_empty()
            && let Err(error) = Topology::new(&topology)
        {
            let errors = leaving(
                SessionError::UnsupportedTopology(error),
                &mut bootloader,
                &format!("unsupported topology: {error}"),
            );
            return Err(refused(journal, errors));
        }
        let handoff = match bootloader.map(BootHandoff::read).transpose() {
            Ok(handoff) => handoff,
            Err(error) => return Err(refused(journal, vec![error.into()])),
        };
        let mut appender =
            match Appender::resume_with(journal, clock, renderer, zone, defects, resume) {
                Ok(appender) => appender,
                Err(unresumed) => {
                    let Unresumed { journal, error } = *unresumed;
                    return Err(refused(journal, vec![error.into()]));
                }
            };
        if let Some(log) = log {
            appender = appender.with_log(log);
        }
        if let Some(hook) = on_append {
            appender = appender.with_on_append(hook);
        }
        if let Some(handoff) = handoff {
            appender = appender.with_handoff(handoff);
        }
        appender.publication = Some(publication);
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
            topology,
            hardware: Hardware::new(CoreSet::default()),
            lifecycle: Lifecycle::Unswept,
            validated: None,
            visit,
            recovery_cancelled: false,
            unreadable: None,
            unconfirmed: Vec::new(),
            shutdown: None,
        })
    }

    #[cfg(test)]
    pub(crate) const fn appender(&self) -> &Appender<'machine> {
        &self.appender
    }

    #[cfg(test)]
    pub(crate) fn cores(&self) -> CoreSet {
        self.hardware.cores()
    }

    pub(crate) const fn same_boot(&self) -> bool {
        matches!(self.visit, Visit::Same { .. })
    }

    pub(crate) const fn unreconciled(&self) -> bool {
        matches!(self.visit, Visit::Same { reconciled: false })
    }

    /// Refuses changed session cores before finishing an earlier boot's dead end; then refuses configured cores the
    /// machine lacks and, unless a dead end stays pending until reconciliation, records the startup kernel boundary
    /// and the configuration.
    pub(crate) fn start(&mut self) -> Outcome {
        let cores: CoreSet = self.topology.iter().map(|info| info.core).collect();
        let recorded = self.appender.recovery();
        if recorded.started() {
            let known: CoreSet = recorded.topology().keys().copied().collect();
            if known != cores {
                return Err(SessionError::TopologyChanged {
                    added: cores.iter().filter(|&core| !known.contains(core)).collect(),
                    removed: known.iter().filter(|&core| !cores.contains(core)).collect(),
                });
            }
        }
        if !self.same_boot()
            && let Some(stop) = self.resume_dead_end()?
        {
            return Ok(Control::Stop(stop));
        }
        if let Some((core, _)) = self
            .config
            .overrides()
            .find(|(core, _)| !cores.contains(*core))
        {
            return Err(SessionError::NoSuchCore { core });
        }
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
            config: self.config.snapshot(),
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
                        (
                            mces,
                            Some(format!(
                                "{}; {}",
                                Error::CursorMissing,
                                togi_render::error_chain(&error)
                            )),
                        )
                    }
                }
            }
            Err(Partial { value, error }) => (value, Some(togi_render::error_chain(&error))),
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
                monotonic_ns: Some(
                    u64::try_from(mce.monotonic.as_nanos()).map_err(SessionError::Monotonic)?,
                ),
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
    pub(crate) fn shut_down(&mut self, stop: Stop) -> Control {
        self.record_shutdown(&stop);
        Control::Stop(stop)
    }

    pub(crate) fn record_shutdown(&mut self, stop: &Stop) {
        let (reason, cycles) = match stop {
            Stop::Signal => (ShutdownReason::Signal, None),
            Stop::DeadEnd { .. } => (ShutdownReason::DeadEnd, None),
            Stop::Cycles => (ShutdownReason::Cycles, self.policy.cycles),
            Stop::FirstResult => (ShutdownReason::FirstResult, None),
        };
        self.shutdown = Some(Shutdown {
            reason,
            kernel_cursor: None,
            kernel_error: None,
            cycles,
        });
    }

    /// The inconclusive trials in a row of one backend after which its retries begin.
    pub(crate) fn inconclusive_threshold(&self) -> DeadEndThreshold {
        self.config.dead_ends().inconclusive_in_a_row()
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
    pub(crate) fn check_dead_end(&mut self) -> Result<Option<Stop>, SessionError> {
        let recovery = self.appender.recovery();
        let cited = |condition, cited: Option<&Cited>| {
            cited.map(|cited| (condition, cited.detail.clone(), vec![cited.seq]))
        };
        let inconclusive = self.inconclusive_threshold();
        let stray_limit = self.config.dead_ends().stray_crashes_in_a_row().get() as usize;
        let unreadable = self.unreadable.as_ref().map(|unreadable| {
            (
                DeadEndCondition::NoEvidence,
                unreadable.detail.clone(),
                unreadable.retries.clone(),
            )
        });
        let found = cited(DeadEndCondition::ThermalTrip, recovery.thermal_evidence())
            .or(unreadable)
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
        self.finish_dead_end(seq, dead_end, causes.to_vec(), None)
    }

    /// Finishes the dead end the journal holds pending, if any.
    pub(crate) fn resume_dead_end(&mut self) -> Result<Option<Stop>, SessionError> {
        let Some(pending) = self.appender.recovery().pending_dead_end() else {
            return Ok(None);
        };
        let (seq, dead_end, causes, cleared) = (
            pending.seq,
            pending.dead_end.clone(),
            pending.causes.clone(),
            pending.saved_entry_cleared(),
        );
        self.finish_dead_end(seq, dead_end, causes, cleared)
            .map(Some)
    }

    /// Leaves the tuning boot for a dead end and records the shutdown it ends the run with. `cleared` is the result
    /// of an earlier attempt to clear the saved boot entry, when there was one.
    fn finish_dead_end(
        &mut self,
        seq: Seq,
        dead_end: DeadEnd,
        causes: Vec<Seq>,
        cleared: Option<bool>,
    ) -> Result<Stop, SessionError> {
        if self.unreconciled() {
            return Ok(dead_end_stop(dead_end, causes, false));
        }
        let action = dead_end.action.unwrap_or(DeadEndAction::Exit);
        let cleared = match (action, cleared) {
            (DeadEndAction::Exit, _) => true,
            (_, Some(cleared)) => cleared,
            (DeadEndAction::ClearSavedEntry | DeadEndAction::ClearSavedEntryAndReboot, None) => {
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
        };
        let reboot = action == DeadEndAction::ClearSavedEntryAndReboot && cleared;
        let stop = dead_end_stop(dead_end, causes, reboot);
        self.record_shutdown(&stop);
        Ok(stop)
    }
}

fn dead_end_stop(dead_end: DeadEnd, causes: Vec<Seq>, reboot: bool) -> Stop {
    Stop::DeadEnd {
        dead_end,
        causes,
        reboot,
    }
}

/// What this build may do with the journal for `operation`, judged by the build of its `session.start`; `None` for a
/// journal that has none.
pub(crate) fn verdict(build: &Build, journal: &Journal, operation: Operation) -> Option<Verdict> {
    let recorded = journal.events().iter().find_map(|event| {
        if let Payload::SessionStart(start) = &event.payload {
            Some(&start.build)
        } else {
            None
        }
    })?;
    Some(build.verdict(recorded, operation))
}

/// Why this build may not append to the journal.
pub(crate) fn refusal(build: &Build, journal: &Journal) -> Option<togi_journal::Refusal> {
    match verdict(build, journal, Operation::Append)? {
        Verdict::Refuse(refusal) => Some(refusal),
        Verdict::Use => None,
        Verdict::Archive => unreachable!("an append is refused or used, never archived"),
    }
}

/// The errors of a run refused before its first append: `refusal`, then those of leaving the tuning boot for `reason`
/// when the run was started from it.
fn leaving(
    refusal: SessionError,
    bootloader: &mut Option<Box<dyn Bootloader>>,
    reason: &str,
) -> Vec<SessionError> {
    let mut errors = vec![refusal];
    if let Some(bootloader) = bootloader.as_deref_mut() {
        let left = boot::leave(bootloader, reason);
        errors.extend(left.reason_error.map(SessionError::from));
        errors.extend(left.entry.err().map(SessionError::ClearSavedEntry));
    }
    errors
}

#[cfg(test)]
mod tests;
