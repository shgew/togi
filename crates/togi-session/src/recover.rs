//! What a run owes the boots before it: every crash since the last clean stop classified and recorded with its
//! evidence, the trial a crash or a stop left open closed, and the idle crashes answered with failures.

use std::time::Duration;

use togi_journal::{
    Attribution, BackendRetry, CrashDetected, Failure, Mce, Outcome, Payload, RetryTarget, Seq,
    SessionWarning, TrialEnd, TrialId,
};
use togi_machine::{
    BootId, CancellationToken, CoreSet, Error, Kernel, PerCore, Regime, ResetReason, Signal,
    TargetOffset, TrialResult, Trials,
};
use togi_tuner::{CrashFacts, CrashKind, classify_crash, recorded_kind};

use crate::appender::Appender;
use crate::emergency::SweepCleared;
use crate::error::{AppendFailure, Flow, SessionError, Step};
use crate::evidence::{self, RETRY_WAITS, RecordedMce, ResetVerdict, TrialEvidence};
use crate::recovery::{RecoveryState, Registers};

/// What recovery works with: the journal through its appender, the kernel log, the trial files, the boot the run is
/// in and the token that cancels a retry wait.
pub struct Recovery<'a> {
    pub appender: &'a mut Appender,
    pub kernel: &'a mut dyn Kernel,
    pub trials: &'a mut dyn Trials,
    pub boot: &'a BootId,
    pub cancel: &'a CancellationToken,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Recovered {
    /// A kernel-log retry wait was cancelled: the crash it was for and the ones after it stay unclassified.
    pub cancelled: bool,
    /// The kernel log stayed unreadable through every retry: the run has no evidence to go on.
    pub unreadable: Option<KernelUnreadable>,
}

/// The kernel log that could not be read, for the no-evidence dead end that cites its retries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelUnreadable {
    pub detail: String,
    pub retries: Vec<Seq>,
}

/// Classifies and records every crashed boot before the current one, oldest first, then closes the trial they left
/// open and answers their idle crashes. A trial open in the current boot waits for [`same_boot_trial`]: only the
/// sweep proves nothing writes its files any more. Repeating it records nothing new.
pub fn prior_boots(recovery: Recovery<'_>) -> Step<Recovered> {
    let mut crashes = Crashes {
        recovery,
        waited: false,
    };
    match crashes.classify() {
        Ok(()) => {}
        Err(Halt::Cancelled) => {
            return Ok(Recovered {
                cancelled: true,
                unreadable: None,
            });
        }
        Err(Halt::Unreadable(unreadable)) => {
            return Ok(Recovered {
                cancelled: false,
                unreadable: Some(unreadable),
            });
        }
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
        return Ok(Recovered::default());
    }
    finish(&mut recovery)?;
    Ok(Recovered::default())
}

/// Closes the trial an earlier run left open in the current boot, once the sweep cleared what it left running. With
/// a dead end pending it only closes the trial: the dead end ends the run before any failure could be acted on.
pub fn same_boot_trial(mut recovery: Recovery<'_>, _cleared: &SweepCleared) -> Step<()> {
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
pub fn append_trial_end(
    appender: &mut Appender,
    trials: &mut dyn Trials,
    end: TrialEnd,
    causes: &[Seq],
) -> Step<Seq> {
    let trial = end.trial.clone();
    let compressed = match end.containment_error {
        Some(_) => Ok(()),
        None => trials.ended(trial.as_str()),
    };
    if let Err(Error::Crashed) = compressed {
        return Err(SessionError::Machine(Error::Crashed).into());
    }
    let seq = appender.append(Payload::TrialEnd(end), causes)?;
    if let Err(error) = compressed {
        appender.append(
            Payload::SessionWarning(SessionWarning {
                operation: "compress trial files".into(),
                trial: Some(trial),
                error: error.to_string(),
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
        let own = self
            .read(crashed, true, |kernel| kernel.mces(crashed, Duration::ZERO))?
            .unwrap_or_default();
        let retained = self
            .read(&next, next != *current, |kernel| {
                kernel.mces(&next, Duration::ZERO)
            })?
            .unwrap_or_default();
        let reason = self
            .read(crashed, true, |kernel| kernel.reset_reason_after(crashed))?
            .unwrap_or_else(unread_reason);
        let confirmed = confirmed || reason.kind.is_some() || self.any_reason()?;

        let open = self
            .state()
            .open_trial()
            .filter(|open| open.boot == *crashed);
        let in_trial = open.is_some();
        let signalled = open.is_some_and(|open| open.failure.is_some());
        let window: Vec<bool> = own
            .iter()
            .map(|mce| open.is_some_and(|open| open.includes_mce(Some(mce.monotonic))))
            .collect();
        let mut evidence = signalled || window.contains(&true);
        for (mce, inside) in own.iter().zip(window) {
            self.record(mce, crashed, !inside)?;
        }
        for mce in retained.iter().filter(|mce| !mce.corrected) {
            evidence = true;
            self.record(mce, &next, false)?;
        }

        let state = self.state();
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
        let causes = state.recorded_for(crashed, current);
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
                    error: error.to_string(),
                }),
                &[seq],
            )?;
        }
        Ok(confirmed)
    }

    /// Whether the kernel of any boot of the journal logged a reset reason: then it reports them, and a crash it
    /// gave no reason for is no reset the hardware would have named.
    fn any_reason(&mut self) -> Result<bool, Halt> {
        let boots = self.state().boots().to_vec();
        for boot in &boots {
            let reason = self.read(boot, true, |kernel| kernel.reset_reason(boot))?;
            if reason.is_some_and(|reason| reason.kind.is_some()) {
                return Ok(true);
            }
        }
        Ok(false)
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
        mut read: impl FnMut(&mut dyn Kernel) -> Result<T, Error>,
    ) -> Result<Option<T>, Halt> {
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
        loop {
            match read(&mut *self.recovery.kernel) {
                Ok(value) => return Ok(Some(value)),
                Err(Error::BootMissing) if missing => return Ok(None),
                Err(Error::Crashed) => return Err(machine(Error::Crashed)),
                Err(error) => self.retry(boot, &error)?,
            }
        }
    }

    fn retry(&mut self, boot: &BootId, error: &Error) -> Result<(), Halt> {
        let retries = self.state().kernel_retries();
        let Some(due) = evidence::kernel_retry(retries.len()) else {
            return Err(Halt::Unreadable(KernelUnreadable {
                detail: format!(
                    "kernel log of boot {boot} unreadable after retries at 1, 5 and 30 min: {error}"
                ),
                retries: retries.to_vec(),
            }));
        };
        self.recovery.appender.append(
            Payload::BackendRetry(BackendRetry {
                backend: RetryTarget::KernelLog,
                attempt: due.attempt,
                wait_s: u32::try_from(due.wait.as_secs()).unwrap_or(u32::MAX),
                reason: format!("read the kernel log of boot {boot}: {error}"),
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

/// Ends the open trial with what the journal and the crash that cut it short prove of it.
fn close_open_trial(recovery: &mut Recovery<'_>) -> Step<()> {
    let state = recovery.appender.recovery();
    let Some(open) = state.open_trial() else {
        return Ok(());
    };
    let current = recovery.boot;
    let signal = open.failure.map(|(signal, _)| signal);
    let mut missing = Some("togi stopped during the trial");
    let mut causes = vec![open.seq];
    let mut mces = Vec::new();
    if let Some(&first) = open.mces.first() {
        mces.push(RecordedMce {
            seq: first,
            corrected: open.corrected,
        });
        causes.extend(&open.mces);
        if signal.is_none() {
            missing = Some("togi stopped during the trial after a machine check");
        }
    }
    let mut interrupted = true;
    let mut stopped = true;
    let mut reset = None;
    let crash = state.crash(&open.boot);
    if let Some((crash_seq, detected)) = crash {
        let recorded = state.recorded_for(&open.boot, current);
        if matches!(signal, Some(Signal::CorrectedMce | Signal::UncorrectedMce)) {
            causes.extend(&recorded);
        }
        if signal.is_some() {
            missing = Some("backend reported a failure before the reset");
            stopped = false;
        }
        let verdict = reset_verdict(detected);
        if signal.is_none() && open.mces.is_empty() {
            causes = std::iter::once(crash_seq).chain(recorded).collect();
            missing = None;
            stopped = false;
            interrupted = verdict.outcome != Outcome::Failure;
        }
        reset = Some(verdict);
    }
    let missing = evidence::join_diagnostic(missing, open.kernel_error.as_deref());
    let result = TrialResult {
        ran: open.ran(),
        ..TrialResult::default()
    };
    let mut verdict = evidence::adjudicate(&TrialEvidence {
        result: &result,
        mces: &mces,
        reset: reset.as_ref(),
        missing: missing.as_deref(),
        duration: Duration::from_secs(open.intent.duration_s.into()),
        containment: None,
    });
    if let Some((signal, core)) = open.failure {
        verdict.outcome = Outcome::Failure;
        verdict.signal = Some(signal);
        verdict.core = core;
        verdict.reason.clone_from(&missing);
    }
    let trial: TrialId = open.intent.trial.clone();
    let mut end = TrialEnd {
        trial: trial.clone(),
        kernel_cursor: state.kernel_cursor(current).cloned(),
        kernel_error: open.kernel_error.clone(),
        outcome: verdict.outcome,
        signal: verdict.signal,
        core: verdict.core,
        duration_s: verdict.duration_s,
        tctl_max_c: verdict.tctl_max_c.and_then(|tctl| i32::try_from(tctl).ok()),
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
        reason: verdict.reason,
        interrupted,
        last_evidence: stopped && open.kernel_error.is_none(),
        escaped: verdict.escaped,
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
    summary.fill(&mut end, crash.is_some());
    append_trial_end(recovery.appender, recovery.trials, end, &causes)?;
    Ok(())
}

/// What the reset after a crash says of the trial it cut short.
fn reset_verdict(detected: &CrashDetected) -> ResetVerdict {
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
    ResetVerdict {
        outcome,
        signal,
        reason: Some(reason.to_owned()),
    }
}

#[cfg(test)]
mod tests;
