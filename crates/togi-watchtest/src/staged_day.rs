//! Short, staged desktop observations between real tuning nights. The elapsed time is injected,
//! not spent running the calendar or simulating an everyday workload.

use std::num::NonZeroU32;
use std::time::Duration;

use togi_journal::{
    Activity, ApplicationId, ApplyCause, CcdVector, ClimbAction, DayAnswer, DayAnswerKind,
    DayApplied, DayBootStart, DayCheckpoint, DayCleanStop, DayOffsetsChanged, DayUncleanEnd,
    DecisionId, Event, EventKind, Journal, MemFs, Payload, QualificationState, Queue, Seq, Stamp,
    StateDir, TrialIntent,
};
use togi_machine::{
    BootId, CancellationToken, CcdProfile, ControllerMode, CoreInfo, EndEvidence, Machine,
    ObservedOffset, PerCore, Purpose, Regime, ResetKind, TargetOffset, TrialSpec,
};
use togi_sim::{History, ScriptedMachine, ScriptedTopology, Simulator, TrialOutcome};
use togi_simrun::Persistence;
use togi_tuner::day::{HOLD_SECS, Invocation, Pause};
use togi_tuner::policy::{DayQuery, Params, State};

use crate::Error;
use crate::cut::cut_trial;
use crate::simulate::{build as fixture_build, simulate};

const MAX_DAY_DECISIONS: usize = 64;

fn outcomes(spec: &TrialSpec, registers: &[TargetOffset]) -> TrialOutcome {
    let ControllerMode::Managed { purpose, .. } = spec.controller;
    if purpose == Purpose::Isolated
        && spec.regime == Regime::R1
        && let Some(&core) = spec.cores.first()
        && registers[core.index()].get() == -10
    {
        return TrialOutcome::ComputationError {
            core,
            at: Duration::from_secs(7),
        };
    }
    TrialOutcome::Pass
}

#[derive(Clone, Copy)]
pub(super) enum Goal {
    Filler,
    Reproduction,
}

impl Goal {
    fn needs(self, intent: &TrialIntent) -> bool {
        match self {
            Self::Filler => intent.task.is_some_and(|task| task.queue == Queue::Filler),
            Self::Reproduction => intent.activity == Some(Activity::Reproduction),
        }
    }
}

pub(super) fn build(goal: Goal) -> Result<Vec<Event>, Error> {
    let script = ScriptedMachine::new(ScriptedTopology::FourCores, TargetOffset::ZERO, outcomes);
    let sim = Simulator::scripted(script, 1, History::default())?;
    let fs = MemFs::new();
    let persistence = Persistence::Memory(fs.clone());
    let night =
        |stop: fn(&Event) -> bool| simulate(sim.clone(), &persistence, NonZeroU32::MIN, stop);
    night(|event| event.kind() == EventKind::TrialStart)?;

    // A single stock day supplies the hold. The next real night must earn the exact qualification.
    let mut stock = Day::boot(&sim, &fs)?;
    let (_, readbacks) = stock.apply()?;
    if readbacks.iter().any(|(_, offset)| offset.0 != 0) {
        return Err(Error::NoCut("the staged stock hold must start at stock"));
    }
    stock.checkpoint(readbacks, Duration::from_secs(HOLD_SECS))?;
    stock.clean_stop()?;

    sim.reboot()?;
    let qualified = night(|event| {
        matches!(&event.payload, Payload::NightQualification(value)
            if matches!(value.state, QualificationState::Complete { .. }))
    })?;
    if !qualified.iter().any(|event| {
        matches!(&event.payload, Payload::NightQualification(value)
            if matches!(value.state, QualificationState::Complete { .. }))
    }) {
        return Err(Error::NoCut(
            "an exact qualification after the staged stock hold",
        ));
    }
    let mut desktop = Day::boot(&sim, &fs)?;
    let (cause, readbacks) = desktop.apply()?;
    if !matches!(cause, ApplyCause::Step { .. })
        || !readbacks.iter().any(|(_, offset)| offset.0 != 0)
    {
        return Err(Error::NoCut("the first qualified nonzero day application"));
    }
    let checkpoint = desktop.checkpoint(readbacks, Duration::from_hours(1))?;
    match goal {
        Goal::Filler => desktop.clean_stop()?,
        Goal::Reproduction => {
            let ended = desktop.journal.boot().clone();
            desktop.close()?;
            // An unsure answer after an unexplained reset is provisional CPU evidence.
            sim.next_reset(ResetKind::Unknown);
            sim.crash();
            let mut successor = Day::boot(&sim, &fs)?;
            successor.answer_unsure(ended, checkpoint, readbacks)?;
            successor.apply()?;
            successor.clean_stop()?;
        }
    }

    sim.reboot()?;
    // Stop when the actual trial starts; keep the journal prefix at the observed milestone,
    // not the subsequent signal teardown the simulator needs to leave the machine at stock.
    let mut target = None;
    let events = simulate(sim, &persistence, NonZeroU32::MIN, move |event| {
        if let Payload::TrialIntent(intent) = &event.payload
            && goal.needs(intent)
        {
            target = Some(intent.trial.clone());
        }
        matches!(&event.payload, Payload::TrialStart(start) if target.as_ref() == Some(&start.trial))
    })?;
    Ok(cut_trial(&events, |intent| goal.needs(intent))?.to_vec())
}

struct Day<'a> {
    sim: &'a Simulator,
    machine: Machine<'static>,
    journal: Journal,
    topology: Vec<CoreInfo>,
    initial: PerCore<ObservedOffset>,
}

impl<'a> Day<'a> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the staged desktop runs against the same simulated journal as the night fixture"
    )]
    fn boot(sim: &'a Simulator, fs: &MemFs) -> Result<Self, Error> {
        sim.reboot()?;
        let mut machine = sim.seams();
        let boot = machine.host.boot_id()?;
        let topology = machine.host.topology()?;
        let initial = topology
            .iter()
            .map(|info| {
                machine
                    .smu
                    .offset(info.core)
                    .map(|offset| (info.core, offset))
            })
            .collect::<Result<_, _>>()?;
        let bios = machine.host.bios_context()?;
        let mut journal = StateDir::simulated(fs.clone())?.open_at(boot, sim.now())?;
        journal.append_day_boot_start(
            stamp(sim),
            DayBootStart {
                build: fixture_build(),
                bios,
                offsets: initial,
                watchdog: None,
            },
            &[],
        )?;
        Ok(Self {
            sim,
            machine,
            journal,
            topology,
            initial,
        })
    }

    fn append(&mut self, payload: impl Into<Payload>, causes: &[Seq]) -> Result<&Event, Error> {
        Ok(self
            .journal
            .append(stamp(self.sim), payload.into(), causes)?)
    }

    fn answer_unsure(
        &mut self,
        boot: BootId,
        checkpoint: Seq,
        offsets: PerCore<ObservedOffset>,
    ) -> Result<(), Error> {
        let reset_reason = self.machine.kernel.reset_reason_after(&boot)?;
        self.append(
            DayUncleanEnd {
                boot: boot.clone(),
                last_checkpoint: Some(checkpoint),
                offsets,
                reset_reason,
                // This fixture stages the end and the operator's answer, not kernel-tail collection.
                evidence: EndEvidence::unknown(),
            },
            &[],
        )?;
        self.append(
            DayAnswer {
                boot,
                offsets,
                answer: DayAnswerKind::Unsure,
                uid: 0,
            },
            &[],
        )?;
        Ok(())
    }

    fn settle(&mut self) -> Result<(), Error> {
        let mut state = State::new(Params::SPEC);
        for event in self.journal.events() {
            state.fold(event)?;
        }
        let query = DayQuery {
            boot: self.journal.boot().clone(),
            invocation: Invocation::ColdBoot {
                pause: Pause::Running,
            },
            now: self.sim.now(),
        };
        let mut count = 0;
        loop {
            let owed = state.day(&query)?;
            if owed.is_empty() {
                return Ok(());
            }
            count += owed.len();
            if count > MAX_DAY_DECISIONS {
                return Err(Error::NoCut("the staged day query did not settle"));
            }
            for decision in owed {
                state.fold(self.append(Payload::from(decision.payload), &decision.causes)?)?;
            }
        }
    }

    fn apply(&mut self) -> Result<(ApplyCause, PerCore<ObservedOffset>), Error> {
        self.settle()?;
        let last = self.journal.events().iter().rev().find_map(|event| {
            if let Payload::DayApplied(applied) = &event.payload {
                Some((event.seq, applied))
            } else {
                None
            }
        });
        let step = self.journal.events().iter().rev().find_map(|event| {
            if let Payload::ClimbDecision(step) = &event.payload
                && step.cold_boot == *self.journal.boot()
                && !matches!(step.decision, ClimbAction::Hold { .. })
            {
                Some((event.seq, &step.proposed.cores))
            } else {
                None
            }
        });
        let (target, cause, causes) = if let Some((seq, proposed)) = step {
            let profile = CcdProfile::of_cores(proposed, &self.topology)
                .ok_or(Error::NoCut("a uniform CCD day proposal"))?;
            (
                CcdVector::expand(profile, &self.topology)?,
                ApplyCause::Step {
                    decision: DecisionId(seq.get()),
                },
                vec![seq],
            )
        } else if let Some((seq, last)) = last {
            (last.target.clone(), ApplyCause::Reapplication {}, vec![seq])
        } else {
            let zero = CcdProfile::zero(self.topology.iter().map(|info| info.ccd))?;
            (
                CcdVector::expand(zero, &self.topology)?,
                ApplyCause::Reapplication {},
                Vec::new(),
            )
        };
        let next = last.map_or(0, |(_, applied)| applied.application.0 + 1);
        let stopped = self
            .machine
            .trials
            .sweep(&CancellationToken::new())?
            .stopped;
        let validated = self.machine.host.validate_smu()?;
        let applied = togi_session::apply_day_profile(
            &mut self.journal,
            &*self.machine.clock,
            &mut *self.machine.smu,
            validated,
            &stopped,
            &self.topology,
            &target,
            &causes,
        )?;
        let transaction_causes = applied.transactions.clone();
        let record = DayApplied {
            application: ApplicationId(next),
            cause,
            target: applied.target,
            readbacks: applied.readbacks,
            transactions: applied.transactions,
            boot: self.journal.boot().clone(),
            mono_ns: self.sim.monotonic().as_nanos().try_into()?,
        };
        self.append(record, &transaction_causes)?;
        Ok((cause, applied.readbacks))
    }

    fn checkpoint(
        &mut self,
        offsets: PerCore<ObservedOffset>,
        elapsed: Duration,
    ) -> Result<Seq, Error> {
        if offsets != self.initial {
            self.append(DayOffsetsChanged { offsets }, &[])?;
        }
        self.machine
            .clock
            .sleep(&CancellationToken::new(), elapsed)?;
        Ok(self
            .append(
                DayCheckpoint {
                    applied_secs: elapsed.as_secs(),
                    load_mix: togi_journal::LoadMix::default(),
                    watchdog: None,
                },
                &[],
            )?
            .seq)
    }

    fn clean_stop(mut self) -> Result<(), Error> {
        self.append(DayCleanStop {}, &[])?;
        self.close()
    }

    fn close(self) -> Result<(), Error> {
        drop(self.journal.close()?);
        Ok(())
    }
}

fn stamp(sim: &Simulator) -> Stamp {
    Stamp {
        time: sim.now(),
        mono: Some(sim.monotonic()),
    }
}
