//! The production Ruleset 13 policy on the calendar (`docs/spec/runtime.md`, Ruleset 13 policy
//! boundary): the simulator plugs into the same pure functions the session and the day runtime call,
//! never a second tuner. The adapter plays the parts of `togi run`, `togi day` and the day applier.
//! It keeps the journal those would write in memory, folds every event it appends into the real
//! [`State`] and executes what the state decides through the machine seams.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use togi_journal::{
    ApplicationId, ApplyCause, Build, CcdVector, DayAnswer, DayAnswerKind, DayApplied,
    DayBootStart, DayCheckpoint, DayCleanStop, DayEventId, DayOffsetsChanged, DayUncleanEnd, Event,
    FixStamp, Payload, SCHEMA, Seq, SessionBaseline, SessionContext, SessionId, SessionStart,
    Shutdown, ShutdownReason, Stamp, TrialId, TrialInstance, TrialIntent, TrialSignal, TrialStart,
    WorkloadContent,
};
use togi_machine::{
    BootId, CancellationToken, Condition, ControllerMode, CoreInfo, EndEvidence, Machine,
    ObservedOffset, Partial, PerCore, Reporter, ResetReason, Sample, Signal, TargetOffset,
    TrialRole, TrialSpec,
};
use togi_tuner::cap::Beta;
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::policy::{
    Action, Contents, DayQuery, EndReason, NightQuery, Params, PolicyPayload, State, TrialPlan,
};

use super::{DayClassification, DayObservation, EndAnswer, Night, Policy, RunError};

/// How a night trial ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Ended {
    /// The trial ran its course: it passed, failed or was adjudicated inconclusive.
    Completed,
    /// The operator ended the night while the trial ran.
    Interrupted,
    /// The caller retained this trial's start as the end of its run.
    Retained,
}

/// The uid the simulated operator answers with.
const OPERATOR_UID: u32 = 1000;

/// How many commitments one query round may owe before the adapter calls the policy unsettled. The
/// policy owes each commitment once per identity, so a round that keeps owing is a policy fault.
const SETTLE_LIMIT: usize = 100_000;

/// The day observer of the boot the machine is running, while one watches it.
enum Observer {
    /// No day boot is running: before the first day, or during a night's tuning boot.
    Off,
    Watching(DayBoot),
}

struct DayBoot {
    boot: BootId,
    /// The active time the observer has accounted for this boot.
    active: Duration,
    last_checkpoint: Option<Seq>,
    /// The offsets the observer last recorded.
    offsets: PerCore<ObservedOffset>,
}

/// What the adapter knows once the session started.
struct Started {
    cores: Vec<CoreInfo>,
    contents: Contents,
}

/// Ruleset 13 at one fallback slope, driving the real policy state.
pub struct Ruleset13 {
    state: State,
    /// How many events the adapter wrote.
    appended: u64,
    /// The events the adapter wrote, kept only when [`Self::recording`] asked for them.
    recorded: Option<Vec<Event>>,
    started: Option<Started>,
    boot: BootId,
    boots: u64,
    observer: Observer,
    applications: u64,
    trials: u64,
}

impl Ruleset13 {
    /// Ruleset 13 whose fallback slope is `multiplier`, the hazard multiplier per CO count (the
    /// specification's 1.5× is `beta = ln 1.5`).
    ///
    /// # Errors
    /// Refuses a multiplier that is not finite and above one.
    pub fn new(multiplier: f64) -> Result<Self, RunError> {
        if !multiplier.is_finite() || multiplier <= 1.0 {
            return Err(RunError::Slope(multiplier));
        }
        let fallback_slope = Beta::new(multiplier.ln()).map_err(|_| RunError::Slope(multiplier))?;
        Ok(Self::with_params(Params {
            fallback_slope,
            ..Params::SPEC
        }))
    }

    /// Ruleset 13 deciding by `params`.
    #[must_use]
    pub fn with_params(params: Params) -> Self {
        Self {
            state: State::new(params),
            appended: 0,
            recorded: None,
            started: None,
            boot: boot_id(0),
            boots: 0,
            observer: Observer::Off,
            applications: 0,
            trials: 0,
        }
    }

    /// The adapter keeping every event it writes, for [`Self::events`]. A long run holds its whole journal
    /// in memory, so only callers that read the journal back ask for it.
    #[must_use]
    pub fn recording(mut self) -> Self {
        self.recorded.get_or_insert_with(Vec::new);
        self
    }

    /// The journal the adapter wrote, in order; `None` unless built [`Self::recording`].
    #[must_use]
    pub fn events(&self) -> Option<&[Event]> {
        self.recorded.as_deref()
    }

    /// The policy state folded from the journal the adapter wrote.
    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }

    fn append(
        &mut self,
        machine: &Machine<'_>,
        payload: Payload,
        causes: &[Seq],
    ) -> Result<Seq, RunError> {
        let seq = self
            .appended
            .checked_add(1)
            .and_then(Seq::new)
            .ok_or(RunError::Calendar("journal sequence"))?;
        let event = Event::new(
            seq,
            Stamp {
                time: machine.clock.now(),
                mono: Some(machine.clock.monotonic()?),
            },
            self.boot.clone(),
            causes.to_vec(),
            payload,
        );
        self.state.fold(&event)?;
        self.appended = seq.get();
        if let Some(recorded) = &mut self.recorded {
            recorded.push(event);
        }
        Ok(seq)
    }

    fn commit(
        &mut self,
        machine: &Machine<'_>,
        payload: PolicyPayload,
        causes: &[Seq],
    ) -> Result<Seq, RunError> {
        self.append(machine, payload.into(), causes)
    }

    /// Starts the session on first use: `session.start`, the firmware context and the verified
    /// all-zero baseline, as `togi run` records them before tuning.
    fn start(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        if self.started.is_some() {
            return Ok(());
        }
        let cores = machine.host.topology()?;
        let session = SessionId::new(machine.clock.now(), None)
            .map_err(|_| RunError::Calendar("session id"))?;
        let mut contents = Vec::new();
        for regime in togi_machine::ALL_REGIMES {
            for workload in togi_machine::workloads(regime) {
                contents.push((workload.id(), machine.trials.content(workload.id())?));
            }
        }
        self.append(
            machine,
            SessionStart {
                build: build(),
                session,
                cores: cores.clone(),
            }
            .into(),
            &[],
        )?;
        let context = SessionContext::from(machine.host.bios_context()?);
        self.append(machine, context.into(), &[])?;
        let offsets = read_offsets(machine, &cores)?;
        self.append(machine, SessionBaseline { offsets }.into(), &[])?;
        self.started = Some(Started {
            cores,
            contents: Contents::new(contents),
        });
        Ok(())
    }

    fn cores(&self) -> Result<Vec<CoreInfo>, RunError> {
        self.started
            .as_ref()
            .map(|started| started.cores.clone())
            .ok_or(RunError::Calendar("the Ruleset 13 session has not started"))
    }

    fn next_boot(&mut self) -> BootId {
        self.boots += 1;
        self.boot = boot_id(self.boots);
        self.boot.clone()
    }

    /// The day boot ends cleanly: its observer is stopped on request.
    fn close_day(&mut self, machine: &Machine<'_>) -> Result<(), RunError> {
        if let Observer::Watching(_) = std::mem::replace(&mut self.observer, Observer::Off) {
            self.append(machine, DayCleanStop {}.into(), &[])?;
        }
        Ok(())
    }

    /// A day boot starts: its observer records the firmware and the offsets it reads back.
    fn boot_day(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        let cores = self.cores()?;
        let boot = self.next_boot();
        let offsets = read_offsets(machine, &cores)?;
        let bios = machine.host.bios_context()?;
        self.append(
            machine,
            DayBootStart {
                build: build(),
                bios,
                offsets,
                watchdog: None,
            }
            .into(),
            &[],
        )?;
        self.observer = Observer::Watching(DayBoot {
            boot,
            active: Duration::ZERO,
            last_checkpoint: None,
            offsets,
        });
        Ok(())
    }

    /// The cold boot's day invocation: commits what the day query owes, staged until it owes
    /// nothing, then applies the boot's step and records the verified application.
    fn settle_day(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        let mut step = None;
        let mut owed_total = 0_usize;
        loop {
            let owed = self.state.day(&DayQuery {
                boot: self.boot.clone(),
                invocation: Invocation::ColdBoot {
                    pause: Pause::Running,
                },
                now: machine.clock.now(),
            })?;
            if owed.is_empty() {
                break;
            }
            owed_total = owed_total.saturating_add(owed.len());
            if owed_total > SETTLE_LIMIT {
                return Err(RunError::Unsettled("the day query kept owing commitments"));
            }
            for decision in owed {
                if let PolicyPayload::DayStep(proposal) = &decision.payload
                    && proposal.cold_boot == self.boot
                {
                    step = Some((proposal.decision, proposal.proposed.clone()));
                }
                self.commit(machine, decision.payload, &decision.causes)?;
            }
        }
        let (cause, target) = if let Some((decision, proposed)) = step {
            (ApplyCause::Step { decision }, proposed)
        } else {
            let applied = self
                .state
                .project()
                .applied
                .ok_or(RunError::Calendar("no committed day profile to reapply"))?
                .to_ccd()
                .ok_or(RunError::Calendar(
                    "a day profile must contain only CCD units",
                ))?;
            let cores = self.cores()?;
            let target = CcdVector::expand(applied, &cores)?;
            (ApplyCause::Reapplication {}, target)
        };
        self.apply(machine, cause, target)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the calendar adapter applies the day profile to simulated registers only"
    )]
    fn apply(
        &mut self,
        machine: &mut Machine<'_>,
        cause: ApplyCause,
        target: CcdVector,
    ) -> Result<(), RunError> {
        let cores = self.cores()?;
        for (core, offset) in target.cores().iter() {
            machine.smu.set_offset(core, offset)?;
        }
        let readbacks = read_offsets(machine, &cores)?;
        if target
            .cores()
            .iter()
            .any(|(core, offset)| readbacks.get(core) != Some(offset.observed()))
        {
            return Err(RunError::Calendar(
                "the day profile did not read back as written",
            ));
        }
        let application = ApplicationId(self.applications);
        self.applications += 1;
        let mono_ns = u64::try_from(machine.clock.monotonic()?.as_nanos())
            .map_err(|_| RunError::Calendar("monotonic nanoseconds"))?;
        self.append(
            machine,
            DayApplied {
                application,
                cause,
                target,
                readbacks,
                transactions: Vec::new(),
                boot: self.boot.clone(),
                mono_ns,
            }
            .into(),
            &[],
        )?;
        let changed = match &self.observer {
            Observer::Watching(day) => day.offsets != readbacks,
            Observer::Off => false,
        };
        if changed {
            self.append(
                machine,
                DayOffsetsChanged { offsets: readbacks }.into(),
                &[],
            )?;
            if let Observer::Watching(day) = &mut self.observer {
                day.offsets = readbacks;
            }
        }
        Ok(())
    }

    /// The observer's checkpoint after `active` more seconds of use.
    fn checkpoint(&mut self, machine: &Machine<'_>, active: Duration) -> Result<Seq, RunError> {
        let Observer::Watching(day) = &mut self.observer else {
            return Err(RunError::Calendar("a day observation outside a day boot"));
        };
        day.active += active;
        let applied_secs = day.active.as_secs();
        let seq = self.append(
            machine,
            DayCheckpoint {
                applied_secs,
                load_mix: togi_journal::LoadMix::default(),
                watchdog: None,
            }
            .into(),
            &[],
        )?;
        if let Observer::Watching(day) = &mut self.observer {
            day.last_checkpoint = Some(seq);
        }
        Ok(seq)
    }

    /// Runs one trial of `plan` the way the session does: intent, the whole profile written and read
    /// back, start, wait, stop and the adjudicated end.
    #[expect(
        clippy::disallowed_methods,
        reason = "the calendar adapter writes trial profiles to simulated registers only"
    )]
    fn run_trial(
        &mut self,
        machine: &mut Machine<'_>,
        plan: &TrialPlan,
        stop_at_start: &mut impl FnMut(&TrialIntent) -> bool,
    ) -> Result<Ended, RunError> {
        let cores = self.cores()?;
        let id = TrialId::from(format!("night-trial-{}", self.trials));
        let rotation = self.trials;
        self.trials += 1;
        let mut content = Vec::new();
        for workload in plan.workloads() {
            content.push(WorkloadContent {
                workload,
                content: machine.trials.content(workload)?,
            });
        }
        let intent = plan.intent(id.clone(), &cores, content)?;
        let retain_start = stop_at_start(&intent);
        let (regime, workload, intended) = (
            intent.regime,
            intent.workload,
            Duration::from_secs(u64::from(intent.duration_s)),
        );
        let profile = intent.profile;
        let intent = self.append(machine, intent.into(), &plan.causes)?;
        for (core, offset) in profile.iter() {
            machine.smu.set_offset(core, offset)?;
        }
        let mut confirmed = PerCore::default();
        for (core, observed) in read_offsets(machine, &cores)?.iter() {
            confirmed.set(
                core,
                TargetOffset::new(observed.0)
                    .map_err(|_| RunError::Calendar("trial readback offset"))?,
            );
        }
        if profile
            .iter()
            .any(|(core, offset)| confirmed.get(core) != Some(offset))
        {
            return Err(RunError::Calendar(
                "the trial profile did not read back as written",
            ));
        }
        let roles = plan.roles(intent.get(), &cores)?;
        let spec = TrialSpec {
            id: id.as_str().to_owned(),
            regime,
            workload,
            condition: Condition::Together,
            cores: TrialRole::cores(&roles),
            cpus: TrialRole::cpus(&roles),
            roles: roles.clone(),
            duration: intended,
            index: 0,
            seed: intent.get(),
            offsets: confirmed,
            controller: ControllerMode::Managed {
                rotation,
                purpose: plan.cell.purpose,
            },
        };
        let cancel = CancellationToken::new();
        let stopped = machine.trials.sweep(&cancel)?.stopped;
        let window_start_ns = u64::try_from(machine.clock.monotonic()?.as_nanos())
            .map_err(|_| RunError::Calendar("monotonic nanoseconds"))?;
        let mut running = machine
            .trials
            .start(&cancel, spec, stopped)
            .map_err(|partial| partial.error)?;
        let started = running.started();
        let scheduled = started.schedule.is_some();
        self.append(
            machine,
            TrialStart {
                trial: id.clone(),
                window_start_ns: Some(window_start_ns),
                scope: started.scope,
                pid: started.pid,
                cpus: started.cpus,
                argv: started.argv,
                files: started.files,
                instances: started
                    .instances
                    .into_iter()
                    .map(|instance| TrialInstance {
                        core: instance.core,
                        cpus: instance.cpus,
                        pid: instance.pid,
                        scope: instance.scope,
                    })
                    .collect(),
                roles,
                controller: None,
            }
            .into(),
            &[intent],
        )?;
        if retain_start {
            let _stopped = running.stop()?;
            machine.trials.ended(id.as_str())?;
            return Ok(Ended::Retained);
        }
        let waited = running.wait(&cancel, &mut Silent);
        let _stopped = running.stop()?;
        machine.trials.ended(id.as_str())?;
        // The calendar's operator ends the night by signal; the runner returns what ran until then.
        let (result, ended) = match waited {
            Ok(result) => (result, Ended::Completed),
            Err(Partial {
                value,
                error: togi_machine::Error::Cancelled,
            }) => (value, Ended::Interrupted),
            Err(partial) => return Err(partial.error.into()),
        };
        if scheduled {
            self.append(
                machine,
                TrialSignal {
                    trial: id.clone(),
                    schedule: None,
                    stops: result.stops,
                    conts: result.conts,
                }
                .into(),
                &[intent],
            )?;
        }
        let end =
            togi_session::adjudicate_trial_end(id, &result, intended, ended == Ended::Interrupted)
                .map_err(|error| {
                    // The backend-only helper can only reject duration or temperature.
                    if let togi_session::SessionError::TrialDuration(_) = error {
                        RunError::Calendar("trial duration")
                    } else {
                        RunError::Calendar("trial temperature")
                    }
                })?;
        self.append(machine, end.into(), &[intent])?;
        Ok(ended)
    }
}

impl Policy for Ruleset13 {
    fn firmware_changed(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        if self.started.is_none() {
            // The session's own context record already carries the new firmware.
            return self.start(machine);
        }
        let context = SessionContext::from(machine.host.bios_context()?);
        self.append(machine, context.into(), &[])?;
        Ok(())
    }

    fn start_day(&mut self, _day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.start(machine)?;
        self.close_day(machine)?;
        self.boot_day(machine)?;
        self.settle_day(machine)
    }

    fn observe_day(
        &mut self,
        _day: u32,
        evidence: &DayObservation,
        machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError> {
        let (active, answer) = match evidence {
            DayObservation::Completed { active } => {
                self.checkpoint(machine, *active)?;
                return Ok(DayClassification::NotCpuSuspect);
            }
            DayObservation::Ended { active, answer } => (*active, *answer),
        };
        let last_checkpoint = self.checkpoint(machine, active)?;
        let Observer::Watching(crashed) = std::mem::replace(&mut self.observer, Observer::Off)
        else {
            return Err(RunError::Calendar("a day ended outside a day boot"));
        };
        // The next boot's observer finds the unclean end. The calendar exposes no reset reason,
        // machine check or kernel log of the end, so every such field reads as unknown.
        self.boot_day(machine)?;
        self.append(
            machine,
            DayUncleanEnd {
                boot: crashed.boot.clone(),
                last_checkpoint: Some(last_checkpoint),
                offsets: crashed.offsets,
                reset_reason: ResetReason {
                    kind: None,
                    raw: Vec::new(),
                    supported: false,
                },
                evidence: EndEvidence::unknown(),
            }
            .into(),
            &[],
        )?;
        let answer = match answer {
            Some(EndAnswer::Cpu) => Some(DayAnswerKind::Frozen),
            Some(EndAnswer::Other) => Some(DayAnswerKind::Graphics),
            None => None,
        };
        if let Some(answer) = answer {
            self.append(
                machine,
                DayAnswer {
                    boot: crashed.boot.clone(),
                    offsets: crashed.offsets,
                    answer,
                    uid: OPERATOR_UID,
                }
                .into(),
                &[],
            )?;
        }
        self.settle_day(machine)?;
        let event = DayEventId::Boot { boot: crashed.boot };
        let classification = self
            .state
            .project()
            .events
            .into_iter()
            .find(|projected| projected.event == event)
            .map(|projected| projected.classification);
        Ok(match classification {
            Some(togi_journal::Classification::CpuSuspect { .. }) => DayClassification::CpuSuspect,
            Some(
                togi_journal::Classification::NonCpu {}
                | togi_journal::Classification::Unexplained {},
            )
            | None => DayClassification::NotCpuSuspect,
        })
    }

    fn night(&mut self, night: &Night, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.night_until_trial_start(night, machine, |_| false)
            .map(|_| ())
    }
}

impl Ruleset13 {
    /// Runs a night until its budget ends or `stop` selects a trial whose start should end the run.
    ///
    /// A selected trial is started and stopped through the machine seams, but no outcome or shutdown
    /// is journaled. The journal retains the open trial, and simulated day offsets are restored.
    /// `Break` means the caller must not run another calendar day.
    ///
    /// # Errors
    /// Returns policy, journal-fold and machine-operation failures, including trial teardown failures.
    #[expect(
        clippy::disallowed_methods,
        reason = "the calendar adapter restores the day profile to simulated registers only"
    )]
    pub fn night_until_trial_start(
        &mut self,
        night: &Night,
        machine: &mut Machine<'_>,
        mut stop_at_start: impl FnMut(&TrialIntent) -> bool,
    ) -> Result<ControlFlow<()>, RunError> {
        self.start(machine)?;
        self.close_day(machine)?;
        self.next_boot();
        let cores = self.cores()?;
        let day_profile = read_offsets(machine, &cores)?;
        let budget = SignedDuration::try_from(night.budget)
            .map_err(|_| RunError::Calendar("night budget"))?;
        let until: Timestamp = machine
            .clock
            .now()
            .checked_add(budget)
            .map_err(|_| RunError::Calendar("night deadline"))?;
        let unavailable = BTreeMap::new();
        let contents = self
            .started
            .as_ref()
            .map(|started| started.contents.clone())
            .ok_or(RunError::Calendar("the Ruleset 13 session has not started"))?;
        let mut decided = 0_usize;
        let mut stop = ShutdownReason::Signal;
        let mut retained = ControlFlow::Continue(());
        // The operator ends the night at `until`; the policy itself ends it only at a dead end.
        while machine.clock.now() < until {
            let action = self.state.next(&NightQuery {
                now: machine.clock.now(),
                contents: &contents,
                unavailable: &unavailable,
            })?;
            match action {
                Action::Decide(decision) => {
                    decided += 1;
                    if decided > SETTLE_LIMIT {
                        return Err(RunError::Unsettled("the night kept deciding"));
                    }
                    self.commit(machine, decision.payload, &decision.causes)?;
                }
                Action::RunTrial(plan) => {
                    decided = 0;
                    match self.run_trial(machine, &plan, &mut stop_at_start)? {
                        Ended::Completed => {}
                        Ended::Interrupted => break,
                        Ended::Retained => {
                            retained = ControlFlow::Break(());
                            break;
                        }
                    }
                }
                Action::EndNight(reason) => {
                    stop = match reason {
                        EndReason::DeadEnd | EndReason::Unavailable => ShutdownReason::DeadEnd,
                    };
                    break;
                }
            }
        }
        // Tuning closes through its shutdown before the next day boot: the operator's signal when the
        // operator ended the night, mid-trial or between trials, else the policy's own stop.
        if retained.is_continue() {
            self.append(
                machine,
                Shutdown {
                    reason: stop,
                    kernel_cursor: None,
                    kernel_error: None,
                    cycles: None,
                }
                .into(),
                &[],
            )?;
        }
        // The tuning boot ends; the next day boot starts from the profile the day held.
        for (core, offset) in day_profile.iter() {
            machine.smu.set_offset(
                core,
                TargetOffset::new(offset.0)
                    .map_err(|_| RunError::Calendar("day readback offset"))?,
            )?;
        }
        Ok(retained)
    }
}

fn read_offsets(
    machine: &mut Machine<'_>,
    cores: &[CoreInfo],
) -> Result<PerCore<ObservedOffset>, RunError> {
    let mut offsets = PerCore::default();
    for info in cores {
        offsets.set(info.core, machine.smu.offset(info.core)?);
    }
    Ok(offsets)
}

fn build() -> Build {
    Build {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        rev: None,
        ruleset: togi_tuner::RULESET,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: togi_tuner::EVIDENCE_EPOCH,
    }
}

fn boot_id(ordinal: u64) -> BootId {
    BootId::from(format!("calendar-boot-{ordinal}"))
}

struct Silent;
impl Reporter for Silent {
    fn progress(&mut self, _detail: &str) {}
    fn sample(&mut self, _sample: Sample) {}
    fn signal(
        &mut self,
        _core: togi_machine::CoreId,
        _signal: Signal,
        _pid: Option<u32>,
        _detail: &str,
    ) {
    }
}

#[cfg(test)]
mod tests;
