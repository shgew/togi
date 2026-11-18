//! Applying capability for the committed profile. The tuner owns every decision.

use togi_hardware::day::Observer;
use togi_journal::{ApplicationId, ApplyCause, CcdVector, DayApplied, Event, Payload, Seq, Stamp};
use togi_machine::{
    BootId, CancellationToken, CcdProfile, Clock, CoreInfo, ObservedOffset, PerCore, Stopped,
    TargetOffset, WatchdogState,
};
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::policy::{DayQuery, Params, State};

use super::Error;

/// The day service owns stock verification; diagnostics only query its durable evidence.
pub(crate) fn current_baseline(
    events: &[Event],
    boot: &BootId,
    topology: &[CoreInfo],
) -> Option<Seq> {
    let mut verified = None;
    let mut readings = PerCore::default();
    let mut last_write = None;
    for event in events.iter().filter(|event| &event.boot == boot) {
        match &event.payload {
            Payload::DayBootStart(start) => {
                readings = PerCore::default();
                last_write = None;
                verified = stock_readback(&start.offsets, topology).then_some(event.seq);
            }
            Payload::SmuIntent(intent) => {
                invalidate_readings(&mut readings, intent.command().core());
                last_write = None;
                verified = None;
            }
            Payload::SmuWrite(write) => {
                invalidate_readings(&mut readings, write.command().core());
                last_write = Some(event.seq);
                verified = None;
            }
            Payload::SmuError(_) | Payload::CommandReset(_) => {
                readings = PerCore::default();
                last_write = None;
                verified = None;
            }
            Payload::DayOffsetsChanged(changed) => {
                if !stock_readback(&changed.offsets, topology) {
                    readings = PerCore::default();
                    last_write = None;
                    verified = None;
                }
            }
            Payload::SmuReadback(readback) => {
                verified = None;
                if readback.offset != ObservedOffset(0)
                    || readback
                        .expected
                        .is_some_and(|expected| expected != TargetOffset::ZERO)
                {
                    readings = PerCore::default();
                    continue;
                }
                let written_zero = readback.expected == Some(TargetOffset::ZERO)
                    && event.cause.iter().any(|seq| {
                        let Ok(index) = events.binary_search_by_key(seq, |event| event.seq) else {
                            return false;
                        };
                        let write = &events[index];
                        Some(write.seq) == last_write
                            && write.seq < event.seq
                            && write.boot == *boot
                            && matches!(&write.payload, Payload::SmuWrite(write)
                                if write.command().offset() == TargetOffset::ZERO
                                    && write.command().core().is_none_or(|core| core == readback.core))
                    });
                readings.set(readback.core, Some((event.seq, written_zero)));
                if !topology.is_empty()
                    && readings.len() == topology.len()
                    && topology.iter().all(|info| {
                        readings
                            .get(info.core)
                            .flatten()
                            .is_some_and(|(_, written_zero)| written_zero)
                    })
                {
                    verified = Some(event.seq);
                }
            }
            Payload::SessionBaseline(baseline) => {
                verified = (stock_readback(&baseline.offsets, topology)
                    && cited_readbacks(event, &readings, topology))
                .then_some(event.seq);
            }
            Payload::ProfileRestored(restored) => {
                verified = (!topology.is_empty()
                    && restored.offsets.len() == topology.len()
                    && topology
                        .iter()
                        .all(|info| restored.offsets.get(info.core) == Some(TargetOffset::ZERO))
                    && cited_readbacks(event, &readings, topology))
                .then_some(event.seq);
            }
            Payload::DayApplied(applied) => {
                if !stock_readback(&applied.readbacks, topology) {
                    verified = None;
                    readings = PerCore::default();
                }
            }
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionImported(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::FrequencyIntent(_)
            | Payload::FrequencyReadback(_)
            | Payload::FrequencyDiscarded(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::TrialIntent(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayCleanStop(_)
            | Payload::DayUncleanEnd(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayFault(_)
            | Payload::ExposureObserved(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightPlan(_)
            | Payload::TaskStart(_)
            | Payload::NightQualification(_)
            | Payload::NightReproduction(_)
            | Payload::DayStep(_)
            | Payload::DayEvent(_)
            | Payload::DayVeto(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayInterval(_)
            | Payload::DayImported(_)
            | Payload::UnitMapping(_)
            | Payload::UnitEdge(_)
            | Payload::ClimbDecision(_)
            | Payload::UnitStop(_)
            | Payload::CampaignStart(_)
            | Payload::CampaignCharge(_)
            | Payload::CampaignReclassified(_)
            | Payload::FirmwareRestoration(_)
            | Payload::SuccessorImported(_)
            | Payload::DayPause(_)
            | Payload::StockAttested(_) => {}
        }
    }
    verified
}

fn invalidate_readings(
    readings: &mut PerCore<Option<(Seq, bool)>>,
    core: Option<togi_machine::CoreId>,
) {
    if let Some(core) = core {
        readings.set(core, None);
    } else {
        *readings = PerCore::default();
    }
}

fn cited_readbacks(
    receipt: &Event,
    readings: &PerCore<Option<(Seq, bool)>>,
    topology: &[CoreInfo],
) -> bool {
    readings.len() == topology.len()
        && topology.iter().all(|info| {
            readings
                .get(info.core)
                .flatten()
                .is_some_and(|(seq, _)| receipt.cause.contains(&seq))
        })
}

fn stock_readback(offsets: &PerCore<ObservedOffset>, topology: &[CoreInfo]) -> bool {
    !topology.is_empty()
        && offsets.len() == topology.len()
        && topology
            .iter()
            .all(|info| offsets.get(info.core) == Some(ObservedOffset(0)))
}

/// A running invocation applies the tuner's committed profile or a pause's stock override.
pub(super) struct Live {
    target: CcdVector,
    stopped: Stopped,
    validated: togi_machine::SmuValidated,
    topology: Vec<togi_machine::CoreInfo>,
    applied: PerCore<ObservedOffset>,
}

impl Live {
    pub(super) fn applied(&self) -> PerCore<ObservedOffset> {
        self.applied
    }

    pub(super) fn preflight(
        journal: &togi_journal::Journal,
        observer: &mut dyn Observer,
    ) -> Result<Self, Error> {
        let topology = super::session_cores(journal.events())?;
        let offsets = observer.offsets()?;
        // The check samples for at most four seconds per candidate device and its verdict gates the
        // first write, so it takes a fresh token and a stop signal deliberately does not interrupt it.
        let watchdog = observer.watchdog(&CancellationToken::new())?;
        if !stock_readback(&offsets, topology) {
            return Err(Error::Baseline);
        }
        if !matches!(watchdog, WatchdogState::Armed { .. }) {
            return Err(Error::Watchdog);
        }
        let profile = CcdProfile::zero(topology.iter().map(|info| info.ccd))?;
        Ok(Self {
            target: CcdVector::expand(profile, topology)?,
            topology: topology.to_vec(),
            stopped: observer.stop_workloads()?,
            validated: observer.validated()?,
            applied: offsets,
        })
    }

    pub(super) fn start(
        &mut self,
        journal: &mut togi_journal::Journal,
        observer: &mut dyn Observer,
        clock: &dyn Clock,
        same_boot: bool,
        pause: Pause,
    ) -> Result<(), Error> {
        let mut policy = replay(journal.events())?;
        let before = journal.events().len();
        let query = DayQuery {
            boot: journal.boot().clone(),
            invocation: if same_boot {
                Invocation::SameBoot
            } else {
                Invocation::ColdBoot { pause }
            },
            now: clock.now(),
        };
        loop {
            let owed = policy.day(&query)?;
            if owed.is_empty() {
                break;
            }
            for decision in owed {
                let event =
                    journal.append(stamp(clock)?, decision.payload.into(), &decision.causes)?;
                policy.fold(event)?;
            }
        }
        if pause == Pause::Paused {
            self.zero(journal, observer, clock)?;
            return Ok(());
        }
        let step = journal
            .events()
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, event)| {
                if let Payload::ClimbDecision(step) = &event.payload {
                    let safety = matches!(
                        step.decision,
                        togi_journal::ClimbAction::Retreat { .. }
                            | togi_journal::ClimbAction::FirmwareZero {}
                    );
                    let applied = journal.events().iter().any(|later| {
                        matches!(&later.payload, Payload::DayApplied(value)
                        if value.cause == ApplyCause::Step {
                            decision: togi_journal::DecisionId(event.seq.get()),
                        })
                    });
                    return (step.cold_boot == *journal.boot()
                        && (!same_boot || index >= before || (safety && !applied)))
                        .then(|| {
                            let profile =
                                CcdProfile::of_cores(&step.proposed.cores, &self.topology)
                                    .ok_or(Error::NonuniformProposal)?;
                            let proposed = CcdVector::expand(profile, &self.topology)?;
                            if proposed.cores() != &step.proposed.cores {
                                return Err(Error::NonuniformProposal);
                            }
                            Ok((
                                event.seq,
                                togi_journal::DecisionId(event.seq.get()),
                                proposed,
                            ))
                        });
                }
                if let Payload::DayStep(step) = &event.payload {
                    (step.cold_boot == *journal.boot() && (!same_boot || index >= before))
                        .then(|| Ok((event.seq, step.decision, step.proposed.clone())))
                } else {
                    None
                }
            })
            .transpose()?;
        let previous = journal.events().iter().rev().find_map(|event| {
            if let Payload::DayApplied(applied) = &event.payload {
                Some((event.seq, applied.target.clone()))
            } else {
                None
            }
        });
        let (requested, cause, causes) = if let Some((seq, decision, proposed)) = step {
            (proposed, ApplyCause::Step { decision }, vec![seq])
        } else if let Some((seq, target)) = previous {
            (target, ApplyCause::Reapplication {}, vec![seq])
        } else {
            (
                self.target.clone(),
                ApplyCause::Reapplication {},
                Vec::new(),
            )
        };
        self.apply(journal, observer, clock, &requested, cause, &causes)
    }

    fn apply(
        &mut self,
        journal: &mut togi_journal::Journal,
        observer: &mut dyn Observer,
        clock: &dyn Clock,
        requested: &CcdVector,
        cause: ApplyCause,
        causes: &[Seq],
    ) -> Result<(), Error> {
        if !is_stock(requested) && journal.events().iter().any(|event| {
            matches!(&event.payload, Payload::DeadEnd(end) if end.condition == togi_journal::DeadEndCondition::Defect)
        }) {
            return Err(Error::Defect);
        }
        let next = journal
            .events()
            .iter()
            .rev()
            .find_map(|event| {
                if let Payload::DayApplied(applied) = &event.payload {
                    Some(applied.application.0)
                } else {
                    None
                }
            })
            .map_or(Some(0), |value| value.checked_add(1))
            .ok_or(Error::ApplicationOverflow)?;
        let verified = togi_session::apply_day_profile(
            journal,
            clock,
            observer,
            self.validated,
            &self.stopped,
            &self.topology,
            requested,
            causes,
        )?;
        self.applied = verified.readbacks;
        let transaction_causes = verified.transactions.clone();
        journal.append(
            stamp(clock)?,
            DayApplied {
                application: ApplicationId(next),
                cause,
                target: verified.target,
                readbacks: verified.readbacks,
                transactions: verified.transactions,
                boot: journal.boot().clone(),
                mono_ns: u64::try_from(clock.monotonic()?.as_nanos())
                    .map_err(|_| Error::DurationOverflow)?,
            }
            .into(),
            &transaction_causes,
        )?;
        Ok(())
    }

    pub(super) fn reapply(
        &mut self,
        journal: &mut togi_journal::Journal,
        observer: &mut dyn Observer,
        clock: &dyn Clock,
    ) -> Result<(), Error> {
        // A reapplication is bounded work that follows a pause or resume the journal already holds, so its
        // watchdog read takes a fresh token: a stop signal during it must not skip the gate and restore a
        // nonzero profile unchecked. A cancelled fresh token cannot occur; an error refuses the reapplication.
        if !matches!(
            observer.watchdog(&CancellationToken::new())?,
            WatchdogState::Armed { .. }
        ) {
            return Err(Error::Watchdog);
        }
        // Firmware invalidation is queried even on resume; SameBoot never promotes.
        self.start(journal, observer, clock, true, Pause::Running)
    }

    pub(super) fn zero(
        &mut self,
        journal: &mut togi_journal::Journal,
        observer: &mut dyn Observer,
        clock: &dyn Clock,
    ) -> Result<PerCore<ObservedOffset>, Error> {
        let verified = togi_session::apply_day_profile(
            journal,
            clock,
            observer,
            self.validated,
            &self.stopped,
            &self.topology,
            &self.target,
            &[],
        )?;
        self.applied = verified.readbacks;
        Ok(verified.readbacks)
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "journal-independent day cleanup delegates only emergency zero after a real stale-scope sweep"
)]
pub(super) fn emergency(observer: &mut dyn Observer) -> Result<(), togi_machine::Error> {
    let stopped = observer.stop_workloads()?;
    let validated = observer.validated()?;
    observer.emergency_zero(validated, &stopped)
}

fn is_stock(target: &CcdVector) -> bool {
    target
        .ccd()
        .iter()
        .all(|(_, value)| value == togi_machine::TargetOffset::ZERO)
}

fn stamp(clock: &dyn Clock) -> Result<Stamp, Error> {
    Ok(Stamp {
        time: clock.now(),
        mono: Some(clock.monotonic()?),
    })
}

fn replay(events: &[Event]) -> Result<State, Error> {
    let mut state = State::new(Params::SPEC);
    for event in events {
        state.fold(event)?;
    }
    Ok(state)
}
