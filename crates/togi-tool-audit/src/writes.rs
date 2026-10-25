use std::collections::{BTreeMap, BTreeSet};

use togi_journal::{DeadEndCondition, Event, Payload, Seq, SmuCommand, SmuReadback};
use togi_machine::{CoreId, ObservedOffset, PerCore};

use crate::auditor::Seen;
use crate::violation::{Check, Findings};

/// The SMU's intent, write and readback protocol, and the register offsets the journal says the hardware holds.
#[derive(Debug, Default)]
pub(crate) struct Writes {
    baseline: PerCore<ObservedOffset>,
    registers: BTreeMap<CoreId, i64>,
    pending: BTreeMap<Seq, BTreeSet<CoreId>>,
    used_intents: BTreeSet<Seq>,
    mismatch: Option<Seq>,
}

impl Writes {
    pub(crate) fn new_boot(&mut self, cores: &[CoreId]) {
        self.pending.clear();
        self.registers.clear();
        self.baseline_registers(cores);
    }

    fn baseline_registers(&mut self, cores: &[CoreId]) {
        for &core in cores {
            if let Some(offset) = self.baseline.get(core) {
                self.registers.insert(core, offset.0);
            }
        }
    }

    pub(crate) fn fold(
        &mut self,
        event: &Event,
        seen: &Seen<'_>,
        cores: &[CoreId],
        findings: &mut Findings,
    ) {
        if let Some(mismatch) = self.mismatch {
            match &event.payload {
                Payload::DeadEnd(dead_end) if dead_end.condition == DeadEndCondition::Smu => {
                    self.mismatch = None;
                }
                Payload::SmuIntent(_)
                | Payload::SmuWrite(_)
                | Payload::TrialStart(_)
                | Payload::ProfileApplied(_) => {
                    findings.add(
                        event.seq,
                        Check::WriteProtocol,
                        format!(
                            "{} continues tuning after the mismatching readback at seq {mismatch}, which must end in an smu dead end",
                            event.kind()
                        ),
                    );
                    self.mismatch = None;
                }
                Payload::SessionStart(_)
                | Payload::SessionContext(_)
                | Payload::SessionBaseline(_)
                | Payload::SessionNotice(_)
                | Payload::SessionWarning(_)
                | Payload::SessionArchived(_)
                | Payload::SessionCarried(_)
                | Payload::ConfigLoaded(_)
                | Payload::PreflightCheck(_)
                | Payload::SmuReadback(_)
                | Payload::SmuError(_)
                | Payload::ProfileChange(_)
                | Payload::ProfileRestored(_)
                | Payload::TrialIntent(_)
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
                | Payload::CommandReset(_)
                | Payload::DefectFound(_)
                | Payload::DefectAnswered(_)
                | Payload::DeadEnd(_)
                | Payload::BootSavedEntry(_)
                | Payload::BootLeaveReason(_)
                | Payload::Shutdown(_)
                | Payload::JournalTorn(_)
                | Payload::DayBootStart(_)
                | Payload::DayCheckpoint(_)
                | Payload::DayOffsetsChanged(_)
                | Payload::DayCleanStop(_)
                | Payload::DayAnswer(_)
                | Payload::DayRecord(_)
                | Payload::DayFault(_)
                | Payload::SessionImported(_)
                | Payload::ExposureObserved(_)
                | Payload::CcdEdge(_)
                | Payload::CcdCap(_)
                | Payload::NightPlan(_)
                | Payload::NightQualification(_)
                | Payload::NightReproduction(_)
                | Payload::DayStep(_)
                | Payload::DayApplied(_)
                | Payload::DayEvent(_)
                | Payload::DayVeto(_)
                | Payload::DayHold(_)
                | Payload::DayFreeze(_)
                | Payload::DayInterval(_)
                | Payload::DayImported(_)
                | Payload::DayUncleanEnd(_)
                | Payload::StateRebuilt(_) => {}
            }
        }
        match &event.payload {
            Payload::SessionBaseline(baseline) => {
                self.baseline = baseline.offsets;
                self.baseline_registers(cores);
            }
            Payload::SmuWrite(record) => {
                self.flush(findings);
                self.write(event, record.command(), seen, cores, findings);
            }
            Payload::SmuReadback(readback) => self.readback(event, readback, seen, findings),
            Payload::ProfileApplied(_) | Payload::ProfileRestored(_) => self.flush(findings),
            Payload::TrialStart(_)
            | Payload::TrialEnd(_)
            | Payload::DeadEnd(_)
            | Payload::Shutdown(_)
            | Payload::SessionArchived(_) => self.flush(findings),
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuError(_)
            | Payload::ProfileChange(_)
            | Payload::TrialIntent(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
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
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayFault(_)
            | Payload::SessionImported(_)
            | Payload::ExposureObserved(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightPlan(_)
            | Payload::NightQualification(_)
            | Payload::NightReproduction(_)
            | Payload::DayStep(_)
            | Payload::DayApplied(_)
            | Payload::DayEvent(_)
            | Payload::DayVeto(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayInterval(_)
            | Payload::DayImported(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
        }
    }

    fn matching_intent(&mut self, event: &Event, command: SmuCommand, seen: &Seen<'_>) -> bool {
        for cause in &event.cause {
            let Some(earlier) = seen.get(cause) else {
                continue;
            };
            if earlier.boot != event.boot || self.used_intents.contains(cause) {
                continue;
            }
            if matches!(&earlier.payload, Payload::SmuIntent(intent) if intent.command() == command)
            {
                self.used_intents.insert(*cause);
                return true;
            }
        }
        false
    }

    fn write(
        &mut self,
        event: &Event,
        command: SmuCommand,
        seen: &Seen<'_>,
        cores: &[CoreId],
        findings: &mut Findings,
    ) {
        if !self.matching_intent(event, command, seen) {
            findings.add(
                event.seq,
                Check::WriteProtocol,
                "SMU write has no matching earlier intent in this boot",
            );
        }
        let written: Vec<CoreId> = command
            .core()
            .map_or_else(|| cores.to_vec(), |core| vec![core]);
        let offset = command.offset().get();
        for &core in &written {
            self.registers.insert(core, offset);
        }
        self.pending
            .insert(event.seq, written.into_iter().collect());
    }

    fn readback(
        &mut self,
        event: &Event,
        readback: &SmuReadback,
        seen: &Seen<'_>,
        findings: &mut Findings,
    ) {
        self.registers.insert(readback.core, readback.offset.0);
        if readback
            .expected
            .is_some_and(|expected| expected.get() != readback.offset.0)
        {
            self.mismatch.get_or_insert(event.seq);
            // The session stops reading at a mismatch, so the mismatch obligation covers the write's other cores.
            for cause in &event.cause {
                self.pending.remove(cause);
            }
        }
        for cause in &event.cause {
            let Some(Payload::SmuWrite(record)) = seen.get(cause).map(|earlier| &earlier.payload)
            else {
                continue;
            };
            let command = record.command();
            if command.core().is_some_and(|core| core != readback.core) {
                findings.add(
                    event.seq,
                    Check::WriteProtocol,
                    format!(
                        "readback of core {} cites the write at seq {cause}, which wrote another core",
                        readback.core
                    ),
                );
            }
            if readback
                .expected
                .is_some_and(|expected| expected != command.offset())
            {
                findings.add(
                    event.seq,
                    Check::WriteProtocol,
                    format!(
                        "readback of core {} expects an offset other than the {} the write at seq {cause} wrote",
                        readback.core,
                        command.offset().get()
                    ),
                );
            }
        }
        self.pending.retain(|_, waiting| {
            waiting.remove(&readback.core);
            !waiting.is_empty()
        });
    }

    pub(crate) fn flush(&mut self, findings: &mut Findings) {
        for (seq, missing) in std::mem::take(&mut self.pending) {
            if missing.is_empty() {
                continue;
            }
            let cores: Vec<String> = missing.iter().map(ToString::to_string).collect();
            findings.add(
                seq,
                Check::WriteProtocol,
                format!(
                    "SMU write has no following readback for cores [{}]",
                    cores.join(", ")
                ),
            );
        }
    }

    /// A finished run must have ended the session at an smu dead end after a readback that differed from its write.
    pub(crate) fn finish(&mut self, findings: &mut Findings) {
        self.flush(findings);
        if let Some(mismatch) = self.mismatch.take() {
            findings.add(
                mismatch,
                Check::WriteProtocol,
                "the run finished without the smu dead end this mismatching readback requires",
            );
        }
    }
}
