//! Applying capability for the committed profile. The tuner owns every decision.

use togi_hardware::day::Observer;
use togi_journal::{ApplicationId, ApplyCause, CcdVector, DayApplied, Event, Payload, Seq, Stamp};
use togi_machine::{CcdProfile, Clock, ObservedOffset, PerCore, Stopped, WatchdogState};
use togi_tuner::day::{Invocation, Pause};
use togi_tuner::policy::{DayQuery, Params, State};

use super::Error;

/// A running invocation applies the tuner's committed profile or a pause's stock override.
pub(super) struct Live {
    target: CcdVector,
    stopped: Stopped,
    validated: togi_machine::SmuValidated,
    topology: Vec<togi_machine::CoreInfo>,
}

impl Live {
    pub(super) fn preflight(
        journal: &togi_journal::Journal,
        observer: &mut dyn Observer,
    ) -> Result<Self, Error> {
        let topology = super::session_cores(journal.events())?;
        let offsets = observer.offsets()?;
        let watchdog = observer.watchdog();
        if offsets.len() != topology.len()
            || topology
                .iter()
                .any(|info| offsets.get(info.core) != Some(ObservedOffset(0)))
        {
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
        &self,
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
        if !matches!(observer.watchdog(), WatchdogState::Armed { .. }) {
            return Err(Error::Watchdog);
        }
        // Firmware invalidation is queried even on resume; SameBoot never promotes.
        self.start(journal, observer, clock, true, Pause::Running)
    }

    pub(super) fn zero(
        &self,
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
