use std::time::Duration;

use togi_machine::{
    CancellationToken, CcdId, CoreId, Machine, PerCore, Reporter, Sample, Signal, TargetOffset,
    TrialSpec,
};

use super::successor::{
    FinishReason, PolicyReport, PolicyUnit, UnitFinish, UnitId, UnitMode, UnitPhase,
};
use super::{DayClassification, DayObservation, Night, Policy, RunError};

/// The stock comparator deliberately performs no night experiments.
pub struct CoZero {
    mode: UnitMode,
    report: PolicyReport,
}

impl Default for CoZero {
    fn default() -> Self {
        Self::new(UnitMode::CcdUniform)
    }
}

impl CoZero {
    pub fn new(mode: UnitMode) -> Self {
        Self {
            mode,
            report: PolicyReport::unavailable(),
        }
    }

    fn initialize(&mut self, day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        if !self.report.unit_membership.is_empty() {
            return Ok(());
        }
        let topology = machine.host.topology()?;
        self.report.readback = Some(
            topology
                .iter()
                .map(|info| {
                    let offset = machine.smu.offset(info.core)?.0;
                    let depth = u8::try_from(-i64::from(offset))
                        .map_err(|_| RunError::Calendar("stock readback depth"))?;
                    Ok((info.core, depth))
                })
                .collect::<Result<_, RunError>>()?,
        );
        let mut ccds: Vec<_> = topology.iter().map(|info| info.ccd).collect();
        ccds.sort_unstable();
        ccds.dedup();
        let groups: Vec<Vec<_>> = match self.mode {
            UnitMode::CcdUniform => ccds
                .iter()
                .map(|ccd| {
                    topology
                        .iter()
                        .filter(|info| info.ccd == *ccd)
                        .map(|info| info.core)
                        .collect()
                })
                .collect(),
            UnitMode::PerCore => topology.iter().map(|info| vec![info.core]).collect(),
        };
        for (index, cores) in groups.into_iter().enumerate() {
            let id = |index| {
                UnitId::new(
                    u16::try_from(index).map_err(|_| RunError::Calendar("stock unit count"))?,
                )
                .map_err(|_| RunError::Calendar("stock unit index"))
            };
            let unit = id(index)?;
            let parent = match self.mode {
                UnitMode::CcdUniform => None,
                UnitMode::PerCore => Some(id(
                    ccds.iter()
                        .position(|ccd| {
                            topology
                                .iter()
                                .any(|info| info.core == cores[0] && info.ccd == *ccd)
                        })
                        .ok_or(RunError::Calendar("stock parent CCD"))?,
                )?),
            };
            self.report.unit_membership.push(PolicyUnit {
                unit,
                cores,
                kind: match self.mode {
                    UnitMode::CcdUniform => UnitPhase::Coarse,
                    UnitMode::PerCore => UnitPhase::Core,
                },
                parent,
            });
            self.report.finishes.push(UnitFinish {
                unit,
                reason: FinishReason::FixedStock,
                day: f64::from(day),
                witnesses: vec!["fixed-stock-policy".to_owned()],
            });
        }
        Ok(())
    }
}

impl Policy for CoZero {
    fn report(&self) -> PolicyReport {
        self.report.clone()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the stock diagnostic comparator writes only simulated registers"
    )]
    fn firmware_changed(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        machine.smu.set_all_offsets(TargetOffset::ZERO)?;
        Ok(())
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the stock diagnostic comparator writes only simulated registers"
    )]
    fn start_day(&mut self, day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        machine.smu.set_all_offsets(TargetOffset::ZERO)?;
        self.initialize(day, machine)
    }

    fn observe_day(
        &mut self,
        _day: u32,
        evidence: &DayObservation,
        _machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError> {
        Ok(evidence.conservative_classification())
    }
    fn night(&mut self, _night: &Night, _machine: &mut Machine<'_>) -> Result<(), RunError> {
        Ok(())
    }
}

/// Observed night edge minus a fixed margin, approached in fixed-size daily steps.
/// No true limit, rate, scenario label, or true crash cause enters this adapter.
pub struct FixedMargin {
    margin: u8,
    step: u8,
    ccds: Vec<Edge>,
    active_credit: Duration,
}

struct Edge {
    ccd: CcdId,
    cores: Vec<CoreId>,
    low: u8,
    high: u8,
    class: usize,
    cap: Option<u8>,
}

impl FixedMargin {
    /// # Errors
    /// Refuses a margin or step outside the writable offset range.
    pub fn new(margin: u8, step: u8) -> Result<Self, RunError> {
        if margin > 50 || step == 0 || step > 50 {
            return Err(RunError::Calendar(
                "fixed margin must be 0..=50 and step 1..=50",
            ));
        }
        Ok(Self {
            margin,
            step,
            ccds: Vec::new(),
            active_credit: Duration::ZERO,
        })
    }

    fn initialize(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        if !self.ccds.is_empty() {
            return Ok(());
        }
        for info in machine.host.topology()? {
            if let Some(edge) = self.ccds.iter_mut().find(|edge| edge.ccd == info.ccd) {
                edge.cores.push(info.core);
            } else {
                self.ccds.push(Edge {
                    ccd: info.ccd,
                    cores: vec![info.core],
                    low: 0,
                    high: 51,
                    class: 0,
                    cap: None,
                });
            }
        }
        self.ccds.sort_by_key(|edge| edge.ccd);
        Ok(())
    }
}

impl Policy for FixedMargin {
    #[expect(
        clippy::disallowed_methods,
        reason = "the fixed-margin diagnostic comparator resets only simulated registers"
    )]
    fn firmware_changed(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.ccds.clear();
        self.active_credit = Duration::ZERO;
        machine.smu.set_all_offsets(TargetOffset::ZERO)?;
        Ok(())
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the fixed-margin diagnostic comparator deploys only simulated offsets"
    )]
    fn start_day(&mut self, _day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.initialize(machine)?;
        if self.active_credit < Duration::from_hours(50) {
            return Ok(());
        }
        let mut promoted = false;
        for edge in &self.ccds {
            if let Some(cap) = edge.cap {
                for &core in &edge.cores {
                    let current = u8::try_from(-machine.smu.offset(core)?.0)
                        .map_err(|_| RunError::Calendar("readback offset magnitude"))?;
                    let next = current.saturating_add(self.step).min(cap);
                    promoted |= next > current;
                    machine
                        .smu
                        .set_offset(core, TargetOffset::clamped(-i64::from(next)))?;
                }
            }
        }
        if promoted {
            self.active_credit = Duration::ZERO;
        }
        Ok(())
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the fixed-margin diagnostic comparator retreats only simulated offsets"
    )]
    fn observe_day(
        &mut self,
        _day: u32,
        evidence: &DayObservation,
        machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError> {
        self.active_credit += match evidence {
            DayObservation::Completed { active } | DayObservation::Ended { active, .. } => *active,
        };
        let classification = evidence.conservative_classification();
        if classification == DayClassification::NotCpuSuspect {
            return Ok(classification);
        }
        // An unattributed interruption cannot identify one CCD. Retreat every deployed CCD.
        for edge in &mut self.ccds {
            for &core in &edge.cores {
                let observed = machine.smu.offset(core)?.0;
                machine
                    .smu
                    .set_offset(core, TargetOffset::clamped(observed + 1))?;
            }
            if let Some(cap) = &mut edge.cap {
                *cap = cap.saturating_sub(1);
            }
        }
        Ok(classification)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the fixed-margin diagnostic comparator probes and restores only simulated offsets"
    )]
    fn night(&mut self, night: &Night, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.initialize(machine)?;
        if night.trials.is_empty() {
            return Ok(());
        }
        let mut deployed = PerCore::default();
        for info in machine.host.topology()? {
            deployed.set(
                info.core,
                TargetOffset::clamped(machine.smu.offset(info.core)?.0),
            );
        }
        let mut remaining = night.budget;
        let outcome = (|| {
            for edge in &mut self.ccds {
                while edge.cap.is_none() {
                    if edge.high - edge.low <= 1 {
                        edge.cap = Some(edge.low.saturating_sub(self.margin));
                        break;
                    }
                    let candidate = edge.low + (edge.high - edge.low) / 2;
                    let mut spec = night.trials[edge.class].clone();
                    if !spec.cores.iter().any(|core| edge.cores.contains(core)) {
                        edge.class += 1;
                        if edge.class == night.trials.len() {
                            edge.low = candidate;
                            edge.class = 0;
                        }
                        continue;
                    }
                    // Preserve covered sustained classes, and lengthen short onset-blind probes.
                    spec.duration = spec.duration.max(Duration::from_mins(30));
                    if remaining < spec.duration {
                        return Ok(());
                    }
                    machine.smu.set_all_offsets(TargetOffset::ZERO)?;
                    for &core in &edge.cores {
                        machine
                            .smu
                            .set_offset(core, TargetOffset::clamped(-i64::from(candidate)))?;
                    }
                    spec.offsets = PerCore::default();
                    for info in machine.host.topology()? {
                        spec.offsets.set(
                            info.core,
                            TargetOffset::clamped(machine.smu.offset(info.core)?.0),
                        );
                    }
                    let (passed, ran) = probe(machine, spec)?;
                    remaining = remaining.saturating_sub(ran);
                    if passed {
                        edge.class += 1;
                        if edge.class == night.trials.len() {
                            edge.low = candidate;
                            edge.class = 0;
                        }
                    } else {
                        edge.high = candidate;
                        edge.class = 0;
                    }
                }
            }
            Ok(())
        })();
        // Night probes never become daytime settings, even when a probe fails.
        for (core, offset) in deployed.iter() {
            machine.smu.set_offset(core, offset)?;
        }
        outcome
    }
}

struct Silent;
impl Reporter for Silent {
    fn progress(&mut self, _detail: &str) {}
    fn sample(&mut self, _sample: Sample) {}
    fn signal(&mut self, _core: CoreId, _signal: Signal, _pid: Option<u32>, _detail: &str) {}
}

fn probe(machine: &mut Machine<'_>, spec: TrialSpec) -> Result<(bool, Duration), RunError> {
    let cancel = CancellationToken::new();
    let stopped = machine.trials.sweep(&cancel)?.stopped;
    let id = spec.id.clone();
    let mut running = machine
        .trials
        .start(&cancel, spec, stopped)
        .map_err(|partial| partial.error)?;
    let result = running.wait(&cancel, &mut Silent);
    let _stopped = running.stop()?;
    machine.trials.ended(&id)?;
    match result {
        Ok(result) => Ok((
            result.failure.is_none() && result.inconclusive.is_none(),
            result.ran,
        )),
        Err(partial) => match partial.error {
            togi_machine::Error::Crashed => Ok((false, partial.value.ran)),
            error @ (togi_machine::Error::BackendMissing
            | togi_machine::Error::Containment(_)
            | togi_machine::Error::BootMissing
            | togi_machine::Error::CursorMissing
            | togi_machine::Error::Cancelled
            | togi_machine::Error::InvalidMonotonic { .. }
            | togi_machine::Error::Other(_)) => Err(error.into()),
        },
    }
}
