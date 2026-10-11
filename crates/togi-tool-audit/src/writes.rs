use std::collections::{BTreeMap, BTreeSet};

use togi_journal::{Event, Payload, Seq, SmuCommand, SmuReadback};
use togi_machine::{CoreId, ObservedOffset, PerCore, TargetOffset};

use crate::auditor::Seen;
use crate::marks::Marks;
use crate::violation::{Check, Findings};

/// The SMU's intent, write and readback protocol, and the register offsets the journal says the hardware holds.
#[derive(Debug, Default)]
pub(crate) struct Writes {
    baseline: PerCore<ObservedOffset>,
    registers: BTreeMap<CoreId, i64>,
    pending: BTreeMap<Seq, BTreeSet<CoreId>>,
    used_intents: BTreeSet<Seq>,
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
        marks: &Marks,
        findings: &mut Findings,
    ) {
        match &event.payload {
            Payload::SessionBaseline(baseline) => {
                self.baseline = baseline.offsets;
                self.baseline_registers(cores);
            }
            Payload::SmuWrite(record) => {
                self.flush(findings);
                self.write(event, record.command(), seen, cores, marks, findings);
            }
            Payload::SmuReadback(readback) => self.readback(event, readback, seen, findings),
            Payload::ProfileApplied(applied) => {
                self.flush(findings);
                applied_profile(event, &applied.offsets, marks, findings);
            }
            Payload::ProfileRestored(restored) => {
                self.flush(findings);
                applied_profile(event, &restored.offsets, marks, findings);
            }
            Payload::TrialStart(_)
            | Payload::TrialEnd(_)
            | Payload::DeadEnd(_)
            | Payload::Shutdown(_)
            | Payload::SessionArchived(_) => self.flush(findings),
            _ => {}
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
        marks: &Marks,
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
        marks.check_profile(event, &self.registers, &written, findings);
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
}

fn applied_profile(
    event: &Event,
    offsets: &PerCore<TargetOffset>,
    marks: &Marks,
    findings: &mut Findings,
) {
    let profile: BTreeMap<CoreId, i64> = offsets
        .iter()
        .map(|(core, offset)| (core, offset.get()))
        .collect();
    let written: Vec<CoreId> = profile.keys().copied().collect();
    marks.check_profile(event, &profile, &written, findings);
}
