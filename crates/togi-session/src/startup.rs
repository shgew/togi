//! The steps between the startup records and the first trial: the machine's preflight, the stale-scope sweep and,
//! on a boot a run already used, the reconciliation of the offsets that run left.

use std::time::Duration;

use togi_journal::{DeadEnd, DeadEndCondition, Payload, PreflightCheck, Seq, SessionWarning};
use togi_machine::{CancellationToken, Error, PerCore, TargetOffset};

use crate::emergency::SweepCleared;
use crate::error::{SessionError, Step};
use crate::recover::{self, Recovered, Recovery};
use crate::session::{Control, Outcome, Session, Stop, Visit};
use crate::write;

const WATCHDOG_WAIT: Duration = Duration::from_secs(30);
const WATCHDOG_STEP: Duration = Duration::from_secs(1);

impl Session {
    /// Classifies and records every crashed boot before this one. A kernel log that stays unreadable is evidence
    /// for a no-evidence dead end, which [`Session::check_dead_end`] places after a thermal trip. A pending dead
    /// end is finished first, so nothing is read for it.
    pub fn recover_prior(&mut self) -> Outcome {
        if self.appender.recovery().pending_dead_end().is_some() {
            return Ok(Control::Continue);
        }
        let recovered = recover::prior_boots(Recovery {
            appender: &mut self.appender,
            kernel: &mut *self.kernel,
            trials: &mut *self.trials,
            boot: &self.boot,
            cancel: &self.cancel,
        });
        let step = recovered.map(|recovered| match recovered {
            Recovered::Done => Control::Continue,
            Recovered::Cancelled if self.unreconciled() => {
                self.recovery_cancelled = true;
                Control::Continue
            }
            Recovered::Cancelled => self.shut_down(Stop::Signal),
            Recovered::Unreadable(unreadable) => {
                self.unreadable = Some(unreadable);
                Control::Continue
            }
        });
        self.settle(step)
    }

    /// Closes the trial this boot's earlier run left open, which needs the proof that nothing runs.
    pub fn recover_same_boot(&mut self) -> Outcome {
        if !self.same_boot() || self.recovery_cancelled {
            return Ok(Control::Continue);
        }
        let Some(sweep) = self.sweep else {
            return Ok(Control::Continue);
        };
        let step = recover::same_boot_trial(
            Recovery {
                appender: &mut self.appender,
                kernel: &mut *self.kernel,
                trials: &mut *self.trials,
                boot: &self.boot,
                cancel: &self.cancel,
            },
            &sweep,
        )
        .map(|()| Control::Continue);
        self.settle(step)
    }

    /// A boot whose earlier run left offsets must reconcile them even when asked to stop, so waits ignore the
    /// cancellation until then.
    fn waiting_token(&self) -> CancellationToken {
        if self.unreconciled() {
            CancellationToken::new()
        } else {
            self.cancel.clone()
        }
    }

    fn record_check(&mut self, check: &str, detail: String, ok: bool) -> Result<Seq, SessionError> {
        let payload = PreflightCheck {
            check: check.to_owned(),
            detail,
            ok,
        };
        Ok(self
            .appender
            .append(Payload::PreflightCheck(payload), &[])?)
    }

    /// Waits for the hardware watchdog when started from the tuning boot, records the machine's checks and the BIOS
    /// context comparison, and ends in a preflight dead end citing the failed ones.
    pub fn preflight(&mut self) -> Outcome {
        let cancel = self.waiting_token();
        if self.appender.handoff().is_some() {
            let control = self.await_watchdog(&cancel)?;
            if control != Control::Continue {
                return Ok(control);
            }
        }
        self.validated = match self.host.validate_smu() {
            Ok(validated) => Some(validated),
            Err(Error::Crashed) => return Err(Error::Crashed.into()),
            Err(error) => {
                let warning = SessionWarning {
                    operation: "validate the SMU interface".into(),
                    trial: None,
                    error: format!(
                        "{error}; a journal failure cannot be answered with an emergency zeroing"
                    ),
                };
                self.appender
                    .append(Payload::SessionWarning(warning), &[])?;
                None
            }
        };
        let mut failed = Vec::new();
        let mut names = Vec::new();
        for check in self.host.preflight() {
            let seq = self.record_check(&check.name, check.detail.clone(), check.ok)?;
            if !check.ok {
                failed.push(seq);
                names.push(format!("{} ({})", check.name, check.detail));
            }
        }
        let recorded = self.appender.recovery().context().cloned();
        if let Some(recorded) = recorded
            && failed.is_empty()
        {
            let current = self.host.bios_context()?;
            let mismatch = recorded.first_difference(&current);
            let detail = mismatch.as_ref().map_or_else(
                || "the BIOS context is the one the session recorded".to_owned(),
                ToString::to_string,
            );
            let seq = self.record_check("bios_context", detail.clone(), mismatch.is_none())?;
            if mismatch.is_some() {
                failed.push(seq);
                names.push(format!("bios_context ({detail})"));
            }
        }
        if failed.is_empty() {
            return Ok(Control::Continue);
        }
        let dead_end = DeadEnd {
            condition: DeadEndCondition::Preflight,
            core: None,
            detail: format!("failed checks: {}", names.join(", ")),
            action: None,
        };
        self.dead_end(dead_end, &failed).map(Control::Stop)
    }

    fn await_watchdog(&mut self, cancel: &CancellationToken) -> Outcome {
        let started = self.appender.clock().monotonic();
        loop {
            if cancel.is_cancelled() {
                return Ok(self.shut_down(Stop::Signal));
            }
            let check = self.host.watchdog();
            let waited = self.appender.clock().monotonic().saturating_sub(started);
            if check.ok || waited >= WATCHDOG_WAIT {
                let detail = format!(
                    "{}; waited {}s for an armed hardware watchdog",
                    check.detail,
                    waited.as_secs()
                );
                let seq = self.record_check("watchdog", detail.clone(), check.ok)?;
                if check.ok {
                    return Ok(Control::Continue);
                }
                let dead_end = DeadEnd {
                    condition: DeadEndCondition::Preflight,
                    core: None,
                    detail: format!("failed checks: watchdog ({detail})"),
                    action: None,
                };
                return self.dead_end(dead_end, &[seq]).map(Control::Stop);
            }
            let step = WATCHDOG_STEP.min(WATCHDOG_WAIT.saturating_sub(waited));
            match self.appender.clock().sleep(cancel, step) {
                Ok(()) => {}
                Err(Error::Cancelled) => return Ok(self.shut_down(Stop::Signal)),
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Clears the trial scopes a stale run may have left, which is what makes the SMU safe to touch. The sweep is
    /// never cancelled: it is the only way to know no workload runs. A failed sweep is a containment dead end that
    /// writes nothing to the SMU.
    pub fn sweep(&mut self) -> Outcome {
        match self.trials.sweep(&CancellationToken::new()) {
            Ok(swept) => {
                self.record_check("trial_scopes", swept.detail.clone(), true)?;
                self.sweep = Some(SweepCleared::of(&swept));
                self.stopped = Some(swept.stopped);
                Ok(Control::Continue)
            }
            Err(Error::Crashed) => Err(Error::Crashed.into()),
            Err(error) => {
                let detail = error.to_string();
                let seq = self.record_check("trial_scopes", detail.clone(), false)?;
                let dead_end = DeadEnd {
                    condition: DeadEndCondition::Containment,
                    core: None,
                    detail,
                    action: None,
                };
                self.dead_end(dead_end, &[seq]).map(Control::Stop)
            }
        }
    }
}

impl Session {
    /// On a boot a run already used: reads the offsets that run left, puts the baseline back as bounded by `held`
    /// and only then lets the boot's cancellation count. A failed read or write is SMU evidence.
    pub fn reconcile(&mut self, held: &PerCore<TargetOffset>) -> Outcome {
        if !self.unreconciled() {
            return Ok(Control::Continue);
        }
        let step = self.read_back_and_restore(held).map(|()| Control::Continue);
        let control = self.settle(step)?;
        if control == Control::Continue {
            self.visit = Visit::Same { reconciled: true };
        }
        Ok(control)
    }

    fn read_back_and_restore(&mut self, held: &PerCore<TargetOffset>) -> Step<()> {
        write::read_all(&mut self.appender, &mut *self.smu, &mut self.hardware, &[])?;
        self.restore(held).map(drop)
    }

    /// What the journal holds that ends the run before it starts: a dead end pending from before, a defect that needs
    /// an answer, a dead end its evidence supports, and a stop that was asked for while the earlier boots' evidence
    /// was read.
    pub fn inspect(&mut self) -> Outcome {
        if let Some(stop) = self.resume_dead_end()? {
            return Ok(Control::Stop(stop));
        }
        if let Some(stop) = self.check_defects()? {
            return Ok(Control::Stop(stop));
        }
        if let Some(stop) = self.check_dead_end()? {
            return Ok(Control::Stop(stop));
        }
        if self.recovery_cancelled {
            return Ok(self.shut_down(Stop::Signal));
        }
        Ok(Control::Continue)
    }
}

#[cfg(test)]
mod tests;
