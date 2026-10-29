//! The session's own records before its first trial: the BIOS context, the all-zero baseline the machine was found
//! at and the day records a fresh transition imports.

use std::collections::HashMap;

use togi_journal::{
    CampaignHistory, CampaignId, CampaignOrigin, CampaignStart, EventKind, ExposureOrigin,
    Payload, Provenance, Seq, SessionBaseline, SessionContext, SuccessorImported,
};
use togi_tuner::policy::successor::{self, Reconstruction};
use togi_machine::CoreSet;

use crate::error::{SessionError, Step};
use crate::session::{Control, Outcome, Session};
use crate::write;

impl Session<'_> {
    /// Records what the session starts from, each piece once however often the run begins: the BIOS context (again
    /// when a resumed run finds a different one), the verified all-zero baseline and the import. A baseline that
    /// cannot be read is SMU evidence; one that is nonzero or incomplete is refused, never clamped.
    pub(crate) fn begin(&mut self) -> Outcome {
        let step = self.record_start().map(|()| Control::Continue);
        self.settle(step)
    }

    fn record_start(&mut self) -> Step<()> {
        self.record_context()?;
        self.record_baseline()?;
        self.record_import()
    }

    /// Records the BIOS context unless the journal's latest `session.context` is the machine's current one: a
    /// resumed run on changed firmware records the new context and goes on.
    fn record_context(&mut self) -> Step<()> {
        let context = self.host.bios_context().map_err(SessionError::from)?;
        if self
            .appender
            .recovery()
            .context()
            .is_some_and(|recorded| recorded.first_difference(&context).is_none())
        {
            return Ok(());
        }
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
        let reading = write::read_all(&mut self.appender, &mut *self.smu, &mut self.hardware)?;
        let missing: CoreSet = self
            .topology
            .iter()
            .map(|info| info.core)
            .filter(|core| reading.offsets.get(*core).is_none())
            .collect();
        if !missing.is_empty() {
            return Err(SessionError::BaselineIncomplete { cores: missing }.into());
        }
        let nonzero: CoreSet = reading
            .offsets
            .iter()
            .filter(|(_, offset)| offset.0 != 0)
            .map(|(core, _)| core)
            .collect();
        if !nonzero.is_empty() {
            return Err(SessionError::BaselineNotZero { cores: nonzero }.into());
        }
        let payload = SessionBaseline {
            offsets: reading.offsets,
        };
        self.appender
            .append(Payload::SessionBaseline(payload), &reading.readbacks)?;
        Ok(())
    }

    /// Commit original-identity imports, then enroll the campaign. Each durable stage is resumable.
    fn record_import(&mut self) -> Step<()> {
        if self.appender.journal().events().iter()
            .any(|event| matches!(event.payload, Payload::CampaignStart(_)))
        {
            return Ok(());
        }
        if self.import.is_none() {
            self.import = crate::prepare::inherit(self.appender.journal()).map_err(SessionError::from)?;
        }
        let Some(import) = self.import.clone() else {
            return self.record_campaign(None, CampaignOrigin::Fresh {}, CampaignHistory::Complete {}, &[]);
        };
        let manifest = import.manifest();
        let reconstruction = manifest.reconstruction();
        let campaign = match &reconstruction {
            Reconstruction::Complete(history) => history.campaign,
            Reconstruction::Unreconstructible { .. } => None,
        }.or_else(|| manifest.sources().first().map(|source| {
            CampaignId(Provenance { session: source.session, seq: Seq::FIRST })
        }));
        if let Some((seq, receipt)) = self.appender.journal().events().iter().find_map(|event| {
            if let Payload::SuccessorImported(receipt) = &event.payload { Some((event.seq, receipt.clone())) } else { None }
        }) {
            return self.record_campaign(campaign, CampaignOrigin::Successor { sources: receipt.sources },
                receipt.campaign, &[seq]);
        }
        let committed_selection = self.appender.journal().events().iter().find_map(|event| {
            if let Payload::SessionImported(receipt) = &event.payload {
                Some(receipt.selected.iter().copied().collect::<std::collections::HashSet<_>>())
            } else { None }
        });
        let mut held: HashMap<Provenance, Seq> = self.appender.journal().events().iter()
            .filter_map(|event| {
                if let Payload::DayImported(record) = &event.payload {
                    Some((record.original, event.seq))
                } else { None }
            }).collect();
        let mut causes = Vec::new();
        let mut night_observations = Vec::new();
        let vetoes = match &reconstruction {
            Reconstruction::Complete(history) => history.vetoes.iter().map(|veto| veto.witness).collect(),
            Reconstruction::Unreconstructible { .. } => Vec::new(),
        };
        let mut selected = Vec::new();
        let mut excluded = Vec::new();
        'records: for record in manifest.records() {
            if committed_selection.as_ref().is_some_and(|selected| !selected.contains(&record.original)) {
                continue;
            }
            let exposure = if record.original_kind == EventKind::ExposureObserved {
                let Ok(event) = successor::decode(record) else { continue; };
                let Payload::ExposureObserved(observed) = event.payload else { continue; };
                for entry in &observed.class.workloads {
                    if self.content(entry.workload).is_err() {
                        excluded.push(togi_journal::ImportedRecordRef {
                            original: record.original, kind: record.original_kind.as_str().to_owned(),
                        });
                        continue 'records;
                    }
                }
                let context = self.host.bios_context().map_err(SessionError::from)?;
                if !manifest.topology(record.original.session).is_some_and(|source| {
                    successor::compatible_night(&observed, source, &self.topology, &context,
                        self.build.evidence, &self.contents)
                }) {
                    excluded.push(togi_journal::ImportedRecordRef {
                        original: record.original, kind: record.original_kind.as_str().to_owned(),
                    });
                    continue;
                }
                night_observations.push(record.original);
                Some(observed)
            } else {
                None
            };
            let seq = if let Some(seq) = held.get(&record.original) {
                *seq
            } else {
                let seq = self.appender.append(Payload::DayImported(record.clone()), &[])?;
                held.insert(record.original, seq);
                seq
            };
            causes.push(seq);
            selected.push(record.original);
            if let Some(mut observed) = exposure {
                let exists = self.appender.journal().events().iter().any(|event| {
                    matches!(&event.payload, Payload::ExposureObserved(held) if held.source == observed.source)
                });
                if !exists {
                    observed.origin = ExposureOrigin::Imported {};
                    self.appender.append(Payload::ExposureObserved(observed), &[seq])?;
                }
            }
        }
        if !self.appender.journal().events().iter()
            .any(|event| matches!(event.payload, Payload::SessionImported(_)))
        {
            let mut commitment = manifest.commitment();
            commitment.selected = selected;
            commitment.excluded.extend(excluded);
            commitment.counts.selected = u64::try_from(commitment.selected.len()).unwrap_or(u64::MAX);
            commitment.counts.imported = commitment.counts.selected;
            commitment.counts.excluded = u64::try_from(commitment.excluded.len()).unwrap_or(u64::MAX);
            self.appender.append(Payload::SessionImported(commitment), &causes)?;
        }
        if let (Some(campaign), Reconstruction::Complete(history)) = (campaign, &reconstruction) {
            self.record_reconstructed_charges(campaign, history, &causes)?;
        }
        let history = reconstruction.history();
        let counts = self.appender.journal().events().iter().find_map(|event| {
            if let Payload::SessionImported(receipt) = &event.payload { Some(receipt.counts) } else { None }
        }).unwrap_or_else(|| manifest.commitment().counts);
        let cause = self.appender.append(Payload::SuccessorImported(SuccessorImported {
            sources: manifest.sources().to_vec(), counts, vetoes, night_observations,
            campaign: history.clone(),
        }), &causes)?;
        // Enrollment is the last durable stage: a crash before it repeats the same original
        // identities, and a crash after it cannot create another allowance.
        self.record_campaign(campaign, CampaignOrigin::Successor { sources: manifest.sources().to_vec() }, history, &[cause])
    }

    fn record_campaign(
        &mut self,
        inherited: Option<CampaignId>,
        origin: CampaignOrigin,
        history: CampaignHistory,
        causes: &[Seq],
    ) -> Step<()> {
        let session = self.appender.journal().session()
            .ok_or(SessionError::NotReady(togi_tuner::policy::NotReady::NoSession))?;
        let next = self.appender.journal().events().last().and_then(|event| event.seq.next())
            .ok_or(SessionError::NotReady(togi_tuner::policy::NotReady::SeqExhausted))?;
        let campaign = inherited.unwrap_or(CampaignId(Provenance { session, seq: next }));
        self.appender.append(Payload::CampaignStart(CampaignStart {
            campaign, allowance: 2, origin, history,
        }), causes)?;
        Ok(())
    }

    /// Preserve recovered raw charges even when a reset excludes their raw tuning observations.
    /// These receipts are campaign history, not application or qualification permission.
    fn record_reconstructed_charges(
        &mut self,
        campaign: CampaignId,
        history: &successor::History,
        causes: &[Seq],
    ) -> Step<()> {
        use togi_journal::{CampaignCharge, CampaignReclassified, Classification, DayEventId, UnitVector};
        for (index, charge) in history.charges.iter().enumerate() {
            let to_event = |identity: &successor::Identity| match identity {
                successor::Identity::Boot(boot) => DayEventId::Boot { boot: boot.clone() },
                successor::Identity::Record(original) => DayEventId::Record { session: original.session, seq: original.seq },
                successor::Identity::Unobserved(_) => {
                    let original = charge.witnesses[0];
                    DayEventId::Record { session: original.session, seq: original.seq }
                }
                successor::Identity::Requalification { session, night } => DayEventId::Record { session: *session, seq: night.0 },
            };
            let event = to_event(&charge.event);
            let existing = self.appender.journal().events().iter().find_map(|held| {
                if matches!(&held.payload, Payload::CampaignCharge(held) if held.campaign == campaign && held.event == event) {
                    Some(held.seq)
                } else { None }
            });
            let certainty = if charge.provisional { togi_journal::Certainty::Provisional } else { togi_journal::Certainty::Definite };
            let seq = if let Some(seq) = existing { seq } else {
                let (togi_journal::ObservedProfile::Ccd { cores, .. } |
                    togi_journal::ObservedProfile::PerCore { cores }) = &charge.profile;
                let cores = cores.iter().map(|(core, offset)| {
                    togi_machine::TargetOffset::new(offset.0).map(|offset| (core, offset))
                }).collect::<Result<Vec<_>, _>>().map_err(|_| {
                    SessionError::NotReady(togi_tuner::policy::NotReady::UnmatchedCell)
                })?.into_iter().collect();
                let mapping = charge.mapping.clone()
                    .ok_or(SessionError::NotReady(togi_tuner::policy::NotReady::UnmatchedCell))?;
                let consumed = u32::try_from(index).ok().and_then(|index| index.checked_add(1))
                    .ok_or(SessionError::NotReady(togi_tuner::policy::NotReady::SeqExhausted))?;
                self.appender.append(Payload::CampaignCharge(CampaignCharge {
                    campaign, event: event.clone(), aliases: charge.aliases.iter().map(to_event).collect(),
                    profile: UnitVector { cores, mapping },
                    phase: charge.phase, unit_phase: charge.unit_phase,
                    provisional: charge.provisional, consumed, remaining: 2_u32.saturating_sub(consumed),
                    exhausted: consumed >= 2,
                }), causes)?
            };
            let reclassified = self.appender.journal().events().iter().any(|held| {
                matches!(&held.payload, Payload::CampaignReclassified(held) if held.campaign == campaign && held.event == event)
            });
            if charge.reclassified && !reclassified {
                self.appender.append(Payload::CampaignReclassified(CampaignReclassified {
                    campaign, event, from: Classification::CpuSuspect { certainty }, to: Classification::NonCpu {},
                }), &[seq])?;
            }
        }
        Ok(())
    }
}
