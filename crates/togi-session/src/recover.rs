//! What a run owes the boots before it: every crash since the last clean stop classified and recorded with its
//! evidence, the trial a crash or a stop left open closed, and the idle crashes answered with failures.

use std::time::Duration;

use togi_journal::{
    Attribution, BackendRetry, CrashDetected, Failure, Mce, Outcome, Payload, RetryTarget, Seq,
    SessionWarning, TrialEnd, TrialId,
};
use togi_machine::{
    BootId, CancellationToken, CoreId, CoreSet, Error, Kernel, PerCore, Regime, ResetReason,
    Signal, TargetOffset, Trials,
};
use togi_tuner::{CrashFacts, CrashKind, classify_crash, recorded_kind};

use crate::appender::Appender;
use crate::emergency::SweepCleared;
use crate::error::{AppendFailure, Flow, SessionError, Step};
use crate::evidence::{self, RETRY_WAITS};
use crate::recovery::{RecoveryState, Registers};

/// What recovery works with: the journal through its appender, the kernel log, the trial files, the boot the run is
/// in and the token that cancels a retry wait.
pub(crate) struct Recovery<'a> {
    pub appender: &'a mut Appender,
    pub kernel: &'a mut dyn Kernel,
    pub trials: &'a mut dyn Trials,
    pub boot: &'a BootId,
    pub cancel: &'a CancellationToken,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Recovered {
    /// Every crashed boot is classified and recorded, and the trial they left open is closed.
    Done,
    /// A kernel-log retry wait was cancelled: the crash it was for and the ones after it stay unclassified.
    Cancelled,
    /// The kernel log stayed unreadable through every retry: the run has no evidence to go on.
    Unreadable(KernelUnreadable),
}

/// The kernel log that could not be read, for the no-evidence dead end that cites its retries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KernelUnreadable {
    pub detail: String,
    pub retries: Vec<Seq>,
}

/// Classifies and records every crashed boot before the current one, oldest first, then closes the trial they left
/// open and answers their idle crashes. A trial open in the current boot waits for [`same_boot_trial`]: only the
/// sweep proves nothing writes its files any more. Repeating it records nothing new.
pub(crate) fn prior_boots(recovery: Recovery<'_>) -> Step<Recovered> {
    let mut crashes = Crashes {
        recovery,
        waited: false,
    };
    match crashes.classify() {
        Ok(()) => {}
        Err(Halt::Cancelled) => return Ok(Recovered::Cancelled),
        Err(Halt::Unreadable(unreadable)) => return Ok(Recovered::Unreadable(unreadable)),
        Err(Halt::Flow(flow)) => return Err(flow),
    }
    let mut recovery = crashes.recovery;
    let boot = recovery.boot;
    if recovery
        .appender
        .recovery()
        .open_trial()
        .is_some_and(|open| open.boot == *boot)
    {
        return Ok(Recovered::Done);
    }
    finish(&mut recovery)?;
    Ok(Recovered::Done)
}

/// Closes the trial an earlier run left open in the current boot, once the sweep cleared what it left running. With
/// a dead end pending it only closes the trial: the dead end ends the run before any failure could be acted on.
pub(crate) fn same_boot_trial(mut recovery: Recovery<'_>, _cleared: SweepCleared) -> Step<()> {
    let state = recovery.appender.recovery();
    if state
        .open_trial()
        .is_none_or(|open| open.boot != *recovery.boot)
    {
        return Ok(());
    }
    if state.pending_dead_end().is_some() {
        return close_open_trial(&mut recovery);
    }
    finish(&mut recovery)
}

/// Appends a trial's end after compressing its files, so a crash between the two leaves the files to compress again;
/// a trial whose containment is unconfirmed keeps its files as they are. A failed compression is a warning the end
/// causes; it never changes the outcome.
pub(crate) fn append_trial_end(
    appender: &mut Appender,
    trials: &mut dyn Trials,
    mut end: TrialEnd,
    samples: Result<evidence::SampleSummary, togi_machine::SampleError>,
    crashed: bool,
    causes: &[Seq],
) -> Step<Seq> {
    let sample_error = match samples {
        Ok(summary) => {
            summary.fill(&mut end, crashed);
            None
        }
        Err(error) => {
            end.tctl_max_c = None;
            Some(error)
        }
    };
    let trial = end.trial.clone();
    let compressed = match end.containment_error {
        Some(_) => Ok(()),
        None => trials.ended(trial.as_str()),
    };
    if let Err(Error::Crashed) = compressed {
        return Err(SessionError::Machine(Error::Crashed).into());
    }
    let seq = appender.append(Payload::TrialEnd(end), causes)?;
    if let Some(error) = sample_error {
        appender.append(
            Payload::SessionWarning(SessionWarning {
                operation: "read conditions samples".into(),
                trial: Some(trial.clone()),
                error: togi_render::error_chain(&error),
            }),
            &[seq],
        )?;
    }
    if let Err(error) = compressed {
        appender.append(
            Payload::SessionWarning(SessionWarning {
                operation: "compress trial files".into(),
                trial: Some(trial),
                error: togi_render::error_chain(&error),
            }),
            &[seq],
        )?;
    }
    Ok(seq)
}

enum Halt {
    Cancelled,
    Unreadable(KernelUnreadable),
    Flow(Flow),
}

impl From<AppendFailure> for Halt {
    fn from(failure: AppendFailure) -> Self {
        Self::Flow(failure.into())
    }
}

fn machine(error: Error) -> Halt {
    Halt::Flow(SessionError::Machine(error).into())
}

struct Crashes<'a> {
    recovery: Recovery<'a>,
    /// Whether this run sat out a retry wait since the last crash was classified: a run that resumes after a recorded
    /// retry waits it out first, and records it again only if the read fails after it.
    waited: bool,
}

impl Crashes<'_> {
    fn state(&self) -> &RecoveryState {
        self.recovery.appender.recovery()
    }

    fn classify(&mut self) -> Result<(), Halt> {
        let current = self.recovery.boot;
        let crashed: Vec<BootId> = self.state().crashed_boots(current).cloned().collect();
        let mut confirmed = false;
        for boot in &crashed {
            confirmed = self.classify_boot(boot, confirmed)?;
        }
        Ok(())
    }

    /// Records the crash of `crashed` and returns whether a kernel has been seen to report reset reasons.
    fn classify_boot(&mut self, crashed: &BootId, confirmed: bool) -> Result<bool, Halt> {
        let current = self.recovery.boot;
        let next = self.state().next_boot(crashed, current).clone();
        let own_error = self.read_mces(crashed, false, true)?;
        let retained_error = self.read_mces(&next, true, next != *current)?;
        let evidence = self.has_evidence(crashed);
        if !evidence && let Some(error) = own_error.or(retained_error) {
            return Err(Halt::Unreadable(error));
        }
        let reason = self
            .read(crashed, true, evidence, |kernel| {
                kernel.reset_reason_after(crashed)
            })?
            .unwrap_or_else(unread_reason);
        let confirmed = confirmed || reason.kind.is_some() || self.any_reason(evidence)?;

        let open = self
            .state()
            .open_trial()
            .filter(|open| open.boot == *crashed);
        let in_trial = open.is_some();

        let state = self.state();
        let causes = state.recorded_for(crashed, current);
        let applied = state.applied(crashed);
        let kind = classify_crash(&CrashFacts {
            in_trial,
            applied: applied.is_some(),
            evidence,
            reason: reason.clone(),
            confirmed,
        });
        let stray = kind == CrashKind::Stray;
        let pstore = self.recovery.kernel.saved_pstore(crashed);
        if let Err(Error::Crashed) = pstore {
            return Err(machine(Error::Crashed));
        }
        let state = self.state();
        let detected = CrashDetected {
            previous_boot: crashed.clone(),
            in_flight: state.latest_unmatched_intent(crashed),
            stray,
            condition: applied.filter(|_| !stray).map(|(_, condition)| condition),
            reset_reason: reason.kind,
            reset_reason_raw: reason.raw,
            inconclusive: matches!(kind, CrashKind::Inconclusive | CrashKind::Thermal),
            pstore: pstore.as_ref().ok().cloned().flatten(),
        };
        let seq = self
            .recovery
            .appender
            .append(Payload::CrashDetected(detected), &causes)?;
        self.waited = false;
        if let Err(error) = pstore {
            self.recovery.appender.append(
                Payload::SessionWarning(SessionWarning {
                    operation: "read saved pstore".into(),
                    trial: None,
                    error: togi_render::error_chain(&error),
                }),
                &[seq],
            )?;
        }
        Ok(confirmed)
    }

    /// Whether the kernel of any boot of the journal logged a reset reason: then it reports them, and a crash it
    /// gave no reason for is no reset the hardware would have named.
    fn any_reason(&mut self, evidence: bool) -> Result<bool, Halt> {
        let boots = self.state().boots().to_vec();
        for boot in &boots {
            let reason = self.read(boot, true, evidence, |kernel| kernel.reset_reason(boot))?;
            if reason.is_some_and(|reason| reason.kind.is_some()) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn has_evidence(&self, crashed: &BootId) -> bool {
        self.state()
            .open_trial()
            .is_some_and(|open| open.boot == *crashed && open.failure.is_some())
            || !self
                .state()
                .recorded_for(crashed, self.recovery.boot)
                .is_empty()
    }

    fn read_mces(
        &mut self,
        from: &BootId,
        retained: bool,
        missing: bool,
    ) -> Result<Option<KernelUnreadable>, Halt> {
        self.wait_due()?;
        loop {
            let (mces, error) = match self.recovery.kernel.mces(from, Duration::ZERO) {
                Ok(mces) => (mces, None),
                Err(partial) => (partial.value, Some(partial.error)),
            };
            for mce in &mces {
                if retained && mce.corrected {
                    continue;
                }
                let between = !retained
                    && self.state().open_trial().is_none_or(|open| {
                        open.boot != *from || !open.includes_mce(Some(mce.monotonic))
                    });
                self.record(mce, from, between)?;
            }
            match error {
                None => return Ok(None),
                Some(Error::BootMissing) if missing => return Ok(None),
                Some(Error::Crashed) => return Err(machine(Error::Crashed)),
                Some(error) => match self.retry(from, &error) {
                    Ok(()) => {}
                    Err(Halt::Unreadable(error)) => return Ok(Some(error)),
                    Err(halt) => return Err(halt),
                },
            }
        }
    }

    /// Records a machine check of `from` unless the journal already has it.
    fn record(
        &mut self,
        mce: &togi_machine::Mce,
        from: &BootId,
        between: bool,
    ) -> Result<(), Halt> {
        if self.state().has_recorded(from, mce) {
            return Ok(());
        }
        self.recovery.appender.append(
            Payload::Mce(Mce {
                cpu: mce.cpu,
                core: mce.core,
                bank: mce.bank,
                bank_type: mce.bank_type,
                corrected: mce.corrected,
                monotonic_ns: Some(u64::try_from(mce.monotonic.as_nanos()).unwrap_or(u64::MAX)),
                from_boot: Some(from.clone()),
                trial: None,
                between_trials: between,
                lines: mce.lines.clone(),
            }),
            &[],
        )?;
        Ok(())
    }

    /// Reads the kernel log of `boot`, retrying after the recorded waits. `None` is a boot the system journal no
    /// longer has, which is no failure when `missing` allows it: nothing will bring it back.
    fn read<T>(
        &mut self,
        boot: &BootId,
        missing: bool,
        evidence: bool,
        mut read: impl FnMut(&mut dyn Kernel) -> Result<T, Error>,
    ) -> Result<Option<T>, Halt> {
        self.wait_due()?;
        loop {
            match read(&mut *self.recovery.kernel) {
                Ok(value) => return Ok(Some(value)),
                Err(Error::BootMissing) if missing => return Ok(None),
                Err(Error::Crashed) => return Err(machine(Error::Crashed)),
                Err(error) => match self.retry(boot, &error) {
                    Err(Halt::Unreadable(_)) if evidence => return Ok(None),
                    result => result?,
                },
            }
        }
    }

    fn wait_due(&mut self) -> Result<(), Halt> {
        if !self.waited
            && let Some(wait) = self
                .state()
                .kernel_retries()
                .len()
                .checked_sub(1)
                .and_then(|recorded| RETRY_WAITS.get(recorded))
        {
            self.wait(*wait)?;
        }
        Ok(())
    }

    fn retry(&mut self, boot: &BootId, error: &Error) -> Result<(), Halt> {
        let retries = self.state().kernel_retries();
        let Some(due) = evidence::kernel_retry(retries.len()) else {
            return Err(Halt::Unreadable(KernelUnreadable {
                detail: format!(
                    "kernel log of boot {boot} unreadable after retries at 1, 5 and 30 min: {}",
                    togi_render::error_chain(error)
                ),
                retries: retries.to_vec(),
            }));
        };
        self.recovery.appender.append(
            Payload::BackendRetry(BackendRetry {
                backend: RetryTarget::KernelLog,
                attempt: due.attempt,
                wait_s: u32::try_from(due.wait.as_secs()).unwrap_or(u32::MAX),
                reason: format!(
                    "read the kernel log of boot {boot}: {}",
                    togi_render::error_chain(error)
                ),
            }),
            &[],
        )?;
        self.wait(due.wait)
    }

    fn wait(&mut self, wait: Duration) -> Result<(), Halt> {
        match self
            .recovery
            .appender
            .clock()
            .sleep(self.recovery.cancel, wait)
        {
            Ok(()) => {
                self.waited = true;
                Ok(())
            }
            Err(Error::Cancelled) => Err(Halt::Cancelled),
            Err(error) => Err(machine(error)),
        }
    }
}

fn unread_reason() -> ResetReason {
    ResetReason {
        kind: None,
        raw: Vec::new(),
        supported: false,
    }
}

/// Closes the open trial, then answers every idle crash with a failure.
fn finish(recovery: &mut Recovery<'_>) -> Step<()> {
    close_open_trial(recovery)?;
    let failures: Vec<(Failure, Vec<Seq>)> = {
        let state = recovery.appender.recovery();
        state
            .idle_crashes()
            .map(|(seq, detected)| {
                let causes = std::iter::once(seq)
                    .chain(state.recorded_for(&detected.previous_boot, recovery.boot))
                    .collect();
                (idle_failure(state, detected), causes)
            })
            .collect()
    };
    for (failure, causes) in failures {
        recovery
            .appender
            .append(Payload::Failure(failure), &causes)?;
    }
    Ok(())
}

/// The R6 failure an idle crash stands for, at the deepest profile the crashed boot's registers may have held. It
/// records that profile only when the journal knows every core's register, and names a core only when, besides,
/// every write was read back and exactly one register is nonzero.
fn idle_failure(state: &RecoveryState, detected: &CrashDetected) -> Failure {
    let registers = state.registers(&detected.previous_boot);
    let profile: Option<PerCore<TargetOffset>> = registers.and_then(|registers| {
        state
            .topology()
            .keys()
            .map(|&core| Some((core, TargetOffset::clamped(registers.offset(core)?.0))))
            .collect()
    });
    let sole = profile
        .as_ref()
        .filter(|_| registers.is_some_and(Registers::certain))
        .and_then(sole_nonzero);
    Failure {
        signal: Signal::Crash,
        attribution: if sole.is_some() {
            Attribution::Attributed
        } else {
            Attribution::Unattributed
        },
        core: sole.map(|(core, _)| core),
        offset: sole.map(|(_, offset)| offset),
        trial: None,
        regime: Some(Regime::R6),
        condition: detected.condition,
        profile,
        known_failure: None,
        reason: None,
        round: None,
    }
}

fn sole_nonzero(profile: &PerCore<TargetOffset>) -> Option<(togi_machine::CoreId, TargetOffset)> {
    let mut nonzero = profile
        .iter()
        .filter(|&(_, offset)| offset != TargetOffset::ZERO);
    let sole = nonzero.next()?;
    nonzero.next().is_none().then_some(sole)
}

/// What the journal holds of an open trial and of the crash that cut it short, for [`Closure::decide`].
struct Facts<'a> {
    trial: Seq,
    backend_failure: Option<(Signal, Option<CoreId>)>,
    machine_checks: &'a [Seq],
    machine_check_corrected: bool,
    crash: Option<Cut<'a>>,
}

/// The crash that ended the trial's boot: its record, the machine checks recorded for it and what its reset says.
struct Cut<'a> {
    seq: Seq,
    recorded: &'a [Seq],
    reset: &'a Reset,
}

/// How an open trial closes. The first evidence that holds decides: a backend failure the journal recorded, a
/// machine check inside the trial, the reset after a crash, and last that togi just stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Closure {
    /// The backend reported a failure; a crash after it changes nothing but why the end is not clean.
    BackendFailure {
        signal: Signal,
        core: Option<CoreId>,
        crashed: bool,
        causes: Vec<Seq>,
    },
    MachineChecks {
        signal: Signal,
        crashed: bool,
        causes: Vec<Seq>,
    },
    /// Nothing but the crash: the reset after it says what happened.
    Crashed {
        reset: Reset,
        causes: Vec<Seq>,
    },
    Stopped {
        causes: Vec<Seq>,
    },
}

/// Everything a trial end takes from its [`Closure`].
struct Ruling {
    outcome: Outcome,
    signal: Option<Signal>,
    core: Option<CoreId>,
    reason: &'static str,
    interrupted: bool,
    /// Whether togi itself was the last thing that saw the trial, so its own last evidence is the trial's.
    stopped: bool,
}

impl Closure {
    fn decide(facts: &Facts<'_>) -> Self {
        let mut causes = vec![facts.trial];
        causes.extend(facts.machine_checks);
        let crashed = facts.crash.is_some();
        if let Some((signal, core)) = facts.backend_failure {
            if let Some(cut) = &facts.crash
                && matches!(signal, Signal::CorrectedMce | Signal::UncorrectedMce)
            {
                causes.extend(cut.recorded);
            }
            return Self::BackendFailure {
                signal,
                core,
                crashed,
                causes,
            };
        }
        if !facts.machine_checks.is_empty()
            || facts
                .crash
                .as_ref()
                .is_some_and(|cut| !cut.recorded.is_empty())
        {
            if let Some(cut) = &facts.crash {
                if facts.machine_checks.is_empty() {
                    causes = vec![cut.seq];
                }
                for &seq in cut.recorded {
                    if !causes.contains(&seq) {
                        causes.push(seq);
                    }
                }
            }
            return Self::MachineChecks {
                signal: evidence::machine_check_signal(facts.machine_check_corrected),
                crashed,
                causes,
            };
        }
        match &facts.crash {
            Some(cut) => Self::Crashed {
                reset: cut.reset.clone(),
                causes: std::iter::once(cut.seq)
                    .chain(cut.recorded.iter().copied())
                    .collect(),
            },
            None => Self::Stopped { causes },
        }
    }

    fn causes(&self) -> &[Seq] {
        match self {
            Self::BackendFailure { causes, .. }
            | Self::MachineChecks { causes, .. }
            | Self::Crashed { causes, .. }
            | Self::Stopped { causes } => causes,
        }
    }

    fn ruling(&self) -> Ruling {
        let failure = |signal, core, reason, crashed: bool| Ruling {
            outcome: Outcome::Failure,
            signal: Some(signal),
            core,
            reason,
            interrupted: true,
            stopped: !crashed,
        };
        match self {
            Self::BackendFailure {
                signal,
                core,
                crashed,
                ..
            } => failure(
                *signal,
                *core,
                if *crashed {
                    "backend reported a failure before the reset"
                } else {
                    "togi stopped during the trial"
                },
                *crashed,
            ),
            Self::MachineChecks {
                signal, crashed, ..
            } => failure(
                *signal,
                None,
                "togi stopped during the trial after a machine check",
                *crashed,
            ),
            Self::Crashed { reset, .. } => Ruling {
                outcome: reset.outcome,
                signal: reset.signal,
                core: None,
                reason: reset.reason,
                interrupted: reset.outcome != Outcome::Failure,
                stopped: false,
            },
            Self::Stopped { .. } => Ruling {
                outcome: Outcome::Inconclusive,
                signal: None,
                core: None,
                reason: "togi stopped during the trial",
                interrupted: true,
                stopped: true,
            },
        }
    }
}

/// Ends the open trial with what the journal and the crash that cut it short prove of it.
fn close_open_trial(recovery: &mut Recovery<'_>) -> Step<()> {
    let state = recovery.appender.recovery();
    let Some(open) = state.open_trial() else {
        return Ok(());
    };
    let current = recovery.boot;
    let crash = state.crash(&open.boot);
    let recorded = state.recorded_for(&open.boot, current);
    let reset = crash.map(|(_, detected)| reset_verdict(detected));
    let closure = Closure::decide(&Facts {
        trial: open.seq,
        backend_failure: open.failure,
        machine_checks: &open.mces,
        machine_check_corrected: open.corrected
            || recorded.iter().any(|&seq| {
                recovery.appender.journal().event(seq).is_some_and(
                    |event| matches!(&event.payload, Payload::Mce(mce) if mce.corrected),
                )
            }),
        crash: crash.zip(reset.as_ref()).map(|((seq, _), reset)| Cut {
            seq,
            recorded: &recorded,
            reset,
        }),
    });
    let ruling = closure.ruling();
    let trial: TrialId = open.intent.trial.clone();
    let end = TrialEnd {
        trial: trial.clone(),
        kernel_cursor: state.kernel_cursor(current).cloned(),
        kernel_error: open.kernel_error.clone(),
        outcome: ruling.outcome,
        signal: ruling.signal,
        core: ruling.core,
        duration_s: u32::try_from(open.ran().as_secs()).unwrap_or(u32::MAX),
        tctl_max_c: None,
        voltage_request_median_v: None,
        voltage_request_min_v: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: std::collections::BTreeMap::new(),
        last_sample_s: None,
        last_sample_tctl_c: None,
        last_sample_min_mhz: None,
        last_sample_max_mhz: None,
        stalled_core: None,
        worker_stalled_ms: None,
        reason: evidence::join_diagnostic(Some(ruling.reason), open.kernel_error.as_deref()),
        interrupted: ruling.interrupted,
        last_evidence: ruling.stopped && open.kernel_error.is_none(),
        escaped: Vec::new(),
        backend_missing: false,
        containment_error: None,
    };
    let loaded: CoreSet = if open.intent.cores.is_empty() {
        open.intent.core.into_iter().collect()
    } else {
        open.intent.cores
    };
    let topology = state.topology();
    let summary = evidence::summarize_samples(
        recovery.trials.samples(trial.as_str()),
        loaded,
        open.intent.regime,
        |core| topology.get(&core).copied(),
    );
    append_trial_end(
        recovery.appender,
        recovery.trials,
        end,
        summary,
        crash.is_some(),
        closure.causes(),
    )?;
    Ok(())
}

/// What the reset after a crash says of the trial it cut short.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Reset {
    outcome: Outcome,
    signal: Option<Signal>,
    reason: &'static str,
}

fn reset_verdict(detected: &CrashDetected) -> Reset {
    let reason = ResetReason {
        kind: detected.reset_reason,
        raw: detected.reset_reason_raw.clone(),
        supported: detected.reset_reason.is_some(),
    };
    let (outcome, signal, reason) =
        match recorded_kind(detected.stray, detected.inconclusive, &reason, true) {
            CrashKind::Thermal => (Outcome::Inconclusive, None, "thermal trip during the trial"),
            CrashKind::Inconclusive => (
                Outcome::Inconclusive,
                None,
                "the machine lost power during the trial",
            ),
            CrashKind::InTrial | CrashKind::Idle | CrashKind::Stray => (
                Outcome::Failure,
                Some(Signal::Crash),
                "machine crashed during the trial",
            ),
        };
    Reset {
        outcome,
        signal,
        reason,
    }
}

#[cfg(test)]
mod tests;
