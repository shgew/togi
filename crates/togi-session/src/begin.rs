//! The session's own records before its first trial: the BIOS context, the baseline the machine was found at, the
//! notice for a nonzero one, the carry from an archived session and the phase every core starts the search in.

use std::collections::HashSet;

use togi_carry::copied_identity;
use togi_journal::{
    CarriedCore, CorePhase, CoreTransition, Notice, Payload, SessionBaseline, SessionContext,
    SessionNotice,
};
use togi_machine::{CoreId, CoreSet, Regime, TargetOffset, workloads};

use togi_config::InitialOverride;

use crate::error::{SessionError, Step};
use crate::session::{Control, Outcome, Session};
use crate::write;

impl Session {
    /// Records what the session starts from, each piece once however often the run begins: the BIOS context, the
    /// baseline and its notice, the carry and a start phase for every core. A baseline that cannot be read is SMU
    /// evidence.
    pub fn begin(&mut self) -> Outcome {
        let step = self.record_start().map(|()| Control::Continue);
        self.settle(step)
    }

    fn record_start(&mut self) -> Step<()> {
        self.record_context()?;
        self.record_baseline()?;
        self.record_notice()?;
        self.record_carry()?;
        self.start_phases()
    }

    fn record_context(&mut self) -> Step<()> {
        if self.appender.recovery().context().is_some() {
            return Ok(());
        }
        let context = self.host.bios_context().map_err(SessionError::from)?;
        let payload = SessionContext {
            bios_version: context.bios_version,
            board: context.board,
            cpu_model: context.cpu_model,
            microcode: context.microcode,
            boost_limit_mhz: context.boost_limit_mhz,
        };
        self.appender
            .append(Payload::SessionContext(payload), &[])?;
        Ok(())
    }

    fn record_baseline(&mut self) -> Step<()> {
        if self.appender.recovery().baseline().is_some() {
            return Ok(());
        }
        let reading = write::read_all(&mut self.appender, &mut *self.smu, &mut self.hardware, &[])?;
        let payload = SessionBaseline {
            offsets: reading.offsets,
        };
        self.appender
            .append(Payload::SessionBaseline(payload), &reading.readbacks)?;
        Ok(())
    }

    fn record_notice(&mut self) -> Step<()> {
        if self.appender.recovery().noticed() {
            return Ok(());
        }
        let Some((baseline, offsets)) = self.appender.recovery().baseline() else {
            return Ok(());
        };
        let cores: CoreSet = offsets
            .iter()
            .filter(|(_, offset)| offset.0 != 0)
            .map(|(core, _)| core)
            .collect();
        if cores.is_empty() {
            return Ok(());
        }
        let notice = SessionNotice {
            notice: Notice::NonzeroBaseline,
            cores,
        };
        self.appender
            .append(Payload::SessionNotice(notice), &[baseline])?;
        Ok(())
    }

    /// Appends the facts the carry brings that the journal does not hold yet, then the `session.carried` that commits
    /// them. Only before any core has a phase.
    fn record_carry(&mut self) -> Step<()> {
        let Some(carry) = self.carry.as_ref() else {
            return Ok(());
        };
        let recovery = self.appender.recovery();
        if recovery.carried().is_some()
            || self
                .topology
                .iter()
                .any(|info| recovery.phase(info.core).is_some())
        {
            return Ok(());
        }
        let Some(context) = recovery.context() else {
            return Ok(());
        };
        let resolved = carry.resolve(context, self.config.backends(), self.build.evidence);
        let cores = self.hardware.cores();
        let copied: HashSet<_> = self
            .appender
            .journal()
            .events()
            .iter()
            .filter_map(|event| copied_identity(&event.payload))
            .collect();
        for payload in resolved.fact_payloads(&copied, cores) {
            self.appender.append(payload, &[])?;
        }
        self.appender.append(
            Payload::SessionCarried(resolved.session_carried(cores)),
            &[],
        )?;
        Ok(())
    }

    fn start_phases(&mut self) -> Step<()> {
        let cores: Vec<CoreId> = self.topology.iter().map(|info| info.core).collect();
        for core in cores {
            let recovery = self.appender.recovery();
            if recovery.phase(core).is_some() {
                continue;
            }
            let Some((baseline_seq, baseline)) = recovery.baseline() else {
                return Ok(());
            };
            let carried = recovery.carried();
            let seed = carried.and_then(|(_, seeds)| seeds.get(&core));
            let observed = baseline.get(core).map_or(0, |offset| offset.0);
            let mut causes = vec![baseline_seq];
            causes.extend(carried.filter(|_| seed.is_some()).map(|(seq, _)| seq));
            let transition = self.first_transition(core, observed, seed);
            self.appender
                .append(Payload::CorePhase(transition), &causes)?;
        }
        Ok(())
    }

    /// Where a core's search starts: a configured candidate solo limit, a configured start offset, a carried
    /// candidate, then the clamped baseline. A carried failure point holds the start one count shallower.
    fn first_transition(
        &self,
        core: CoreId,
        observed: i64,
        seed: Option<&CarriedCore>,
    ) -> CoreTransition {
        let clamped = TargetOffset::clamped(observed);
        let carried_limit = seed.and_then(|seed| seed.candidate_solo_limit);
        let (mut start, mut reason, check) = match (self.config.override_for(core), carried_limit) {
            (Some(InitialOverride::CandidateSoloLimit(offset)), _) => {
                (offset, "configured candidate solo limit".to_owned(), true)
            }
            (Some(InitialOverride::Start(offset)), _) => {
                (offset, "configured start offset".to_owned(), false)
            }
            (None, Some(limit)) => (
                limit.offset,
                format!(
                    "candidate solo limit {} carried from session {}",
                    limit.offset.get(),
                    limit.from.session
                ),
                true,
            ),
            (None, None) if clamped.get() == observed => (clamped, "baseline".to_owned(), false),
            (None, None) => (
                clamped,
                format!("baseline {observed} clamped to {}", clamped.get()),
                false,
            ),
        };
        let failure_point = seed.and_then(|seed| seed.failure_point);
        if let Some(point) = failure_point
            && point.offset.get() < 0
            && start <= point.offset
        {
            start = TargetOffset::clamped(point.offset.get() + 1);
            reason = format!(
                "{reason}; clamped to {}, one count shallower than the failure point {} carried from session {}",
                start.get(),
                point.offset.get(),
                point.from.session
            );
        }
        let frozen = [Regime::R1, Regime::R2].map(|regime| workloads(regime)[0].id());
        CoreTransition {
            core,
            from: None,
            to: CorePhase::Search,
            offset: start,
            pass: None,
            failure_point: failure_point.map(|point| point.offset),
            check_solo_limit: check,
            workloads: if check { frozen.to_vec() } else { Vec::new() },
            cleared_combination: Vec::new(),
            reason,
        }
    }
}

#[cfg(test)]
mod tests;
