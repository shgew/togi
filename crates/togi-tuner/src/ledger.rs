use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::path::Path;

use togi_journal::{
    Attribution, Event, Failure, FailureCarried, Outcome, Seq, TrialCarried, TrialClass,
    TunerWarning, WarningKind,
};
use togi_machine::{Condition, CoreId, Regime, WorkloadId, workloads};

use crate::failures::FailureRecord;
use crate::flight::OpenTrial;
use crate::state::Run;
use crate::{
    BackendId, ClassKey, Evidence, FoldError, Observation, Profile, Secs, SessionRef, Window,
};

/// A monotonicity warning the tuner has yet to record, with the events it cites.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingWarning {
    pub(crate) warning: TunerWarning,
    pub(crate) cause: Vec<Seq>,
}

/// `"; N carried facts from session A, B"` for the carried events among `seqs`, or nothing.
pub(crate) fn carried_clause(evidence: &Evidence, seqs: &[Seq]) -> String {
    let sources: Vec<&SessionRef> = seqs
        .iter()
        .filter_map(|&seq| evidence.carried_source(seq))
        .collect();
    if sources.is_empty() {
        return String::new();
    }
    let sessions: BTreeSet<&str> = sources.iter().map(|source| source.as_str()).collect();
    let names: Vec<&str> = sessions.into_iter().collect();
    format!(
        "; {} carried facts from session {}",
        sources.len(),
        names.join(", ")
    )
}

pub(crate) fn profile_text(profile: &Profile) -> String {
    let parts: Vec<String> = profile
        .iter()
        .map(|(core, offset)| format!("{:02}:{}", core.get(), offset.get()))
        .collect();
    format!("[{}]", parts.join(" "))
}

fn source_ref(source: &togi_journal::FactSource) -> SessionRef {
    SessionRef::new(source.session.to_string())
}

impl Run {
    pub(crate) fn idle_class(&self) -> ClassKey {
        ClassKey::new(
            WorkloadId::new(Regime::R6, 0).unwrap_or_else(|| workloads(Regime::R6)[0].id()),
            self.topo.all(),
            self.config.checking_idle,
        )
    }

    fn warn(&mut self, seq: Seq, warning: TunerWarning) {
        let mut cause = vec![seq];
        cause.extend(
            warning
                .passes
                .iter()
                .copied()
                .filter(|&pass| self.evidence.carried_source(pass).is_some()),
        );
        let mut warning = warning;
        warning
            .detail
            .push_str(&carried_clause(&self.evidence, &cause));
        self.warning = Some(PendingWarning { warning, cause });
    }

    fn warn_failure(
        &mut self,
        seq: Seq,
        class: ClassKey,
        profile: &Profile,
        trial: Option<&togi_journal::TrialId>,
    ) {
        if class.multi_r7() {
            return;
        }
        let needed = self.config.needed;
        let count = self.evidence.count(
            class,
            profile,
            Window::LiveOrCarried { since: None },
            needed,
        );
        if count.valid < u32::from(needed.get()) {
            return;
        }
        let detail = format!(
            "failure at profile {} contradicts {} valid passes in {} {}",
            profile_text(profile),
            count.valid,
            class.regime(),
            class.workload()
        );
        self.warn(
            seq,
            TunerWarning {
                warning: WarningKind::Monotonicity,
                trial: trial.cloned(),
                passes: count.witnesses,
                detail,
            },
        );
    }

    fn admit_idle(&mut self, seq: Seq, profile: &Profile, source: Option<SessionRef>) {
        if let Some(found) = self.evidence.idle_monotonicity(profile, self.config.needed) {
            let detail = format!(
                "idle failure at profile {} contradicts {} valid passes in {} {} at {}s",
                profile_text(profile),
                self.config.needed,
                found.class.regime(),
                found.class.workload(),
                found.class.duration().get()
            );
            self.warn(
                seq,
                TunerWarning {
                    warning: WarningKind::Monotonicity,
                    trial: None,
                    passes: found.passes,
                    detail,
                },
            );
        }
        match source {
            Some(source) => self
                .evidence
                .record_carried_idle_failure(seq, profile, source),
            None => self.evidence.record_idle_failure(seq, profile),
        }
    }

    /// Enters a conclusive end into the ledger, warning first when a failure contradicts `n`
    /// valid passes.
    pub(crate) fn record_end(&mut self, seq: Seq, open: &OpenTrial, end: &togi_journal::TrialEnd) {
        let observation = Observation {
            seq,
            class: open.class,
            profile: &open.profile,
            condition: open.intent.condition,
            named: end.core,
        };
        match end.outcome {
            Outcome::Inconclusive => {}
            Outcome::Pass => self.evidence.record_pass(observation),
            Outcome::Failure => {
                self.warn_failure(seq, open.class, &open.profile, Some(&open.trial));
                self.evidence.record_failure(observation);
            }
        }
    }

    pub(crate) fn class_of_trial_class(&self, class: &TrialClass) -> Result<ClassKey, FoldError> {
        let workload = match class.workload {
            Some(workload) if workload.regime() == class.regime => workload,
            Some(workload) => {
                return Err(FoldError::WorkloadRegime {
                    regime: class.regime,
                    workload,
                });
            }
            None => workloads(class.regime)[0].id(),
        };
        if let Some(core) = class.cores.iter().find(|&core| !self.topo.contains(core)) {
            return Err(FoldError::UnknownCore(core));
        }
        let duration = NonZeroU32::new(class.duration_s).ok_or(FoldError::ZeroDuration)?;
        Ok(ClassKey::new(workload, class.cores, Secs::new(duration)))
    }

    pub(crate) fn trial_carried(
        &mut self,
        event: &Event,
        carried: &TrialCarried,
    ) -> Result<(), FoldError> {
        let class = self.class_of_trial_class(&carried.class)?;
        let profile = Profile::new(&self.topo, carried.profile)?;
        if carried.outcome == Outcome::Failure && carried.signal.is_none() {
            return Err(FoldError::MissingSignal);
        }
        if let Some(core) = carried.core {
            self.require_core(core)?;
        }
        let backend: BackendId = match &carried.source.backend {
            Some(identity) => self
                .evidence
                .backend_id(identity.backend, Some(Path::new(&identity.path))),
            None => self.evidence.backend_id(class.workload().backend(), None),
        };
        let source = source_ref(&carried.source);
        let observation = Observation {
            seq: event.seq,
            class,
            profile: &profile,
            condition: carried.condition,
            named: carried.core,
        };
        match carried.outcome {
            Outcome::Inconclusive => {}
            Outcome::Pass => self
                .evidence
                .record_carried_pass(observation, backend, source),
            Outcome::Failure => {
                self.warn_failure(event.seq, class, &profile, carried.source.trial.as_ref());
                let observation = Observation {
                    seq: event.seq,
                    class,
                    profile: &profile,
                    condition: carried.condition,
                    named: carried.core,
                };
                self.evidence.record_carried_failure(observation, source);
                let alone = (carried.condition == Condition::Alone && class.loaded().len() == 1)
                    .then(|| class.loaded().first())
                    .flatten();
                let named: Option<CoreId> = carried.core.or(alone);
                let (attribution, offset) = match named {
                    Some(core) => (Attribution::Attributed, Some(profile.get(core))),
                    None => (Attribution::Unattributed, None),
                };
                let signal = carried.signal.ok_or(FoldError::MissingSignal)?;
                self.failures.insert(FailureRecord {
                    seq: event.seq,
                    failure: Failure {
                        signal,
                        attribution,
                        core: named,
                        offset,
                        trial: carried.source.trial.clone(),
                        regime: Some(class.regime()),
                        condition: Some(carried.condition),
                        profile: Some(profile.offsets()),
                        known_failure: None,
                        reason: None,
                        round: None,
                    },
                    class,
                    profile,
                    carried: true,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn failure_carried(
        &mut self,
        event: &Event,
        carried: &FailureCarried,
    ) -> Result<(), FoldError> {
        let profile = Profile::new(
            &self.topo,
            carried.failure.profile.ok_or(FoldError::NoProfile)?,
        )?;
        let class = self.idle_class();
        let source = source_ref(&carried.source);
        self.admit_idle(event.seq, &profile, Some(source));
        self.failures.insert(FailureRecord {
            seq: event.seq,
            failure: carried.failure.clone(),
            class,
            profile,
            carried: true,
        });
        Ok(())
    }

    pub(crate) fn live_idle_failure(&mut self, seq: Seq, profile: &Profile) {
        self.admit_idle(seq, profile, None);
    }
}
