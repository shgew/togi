use std::collections::{BTreeMap, BTreeSet};

use togi_journal::{
    CommandReset, DefectAnswer, DefectAnswered, DefectDirection, DefectFound, Event, EventKind,
    Payload, ResetTarget, Seq, TunerDecision,
};
use togi_machine::{CoreId, CoreSet};

use crate::registry::{DefectId, Entry, KNOWN_DEFECTS, is_ordered};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Matches {
    cores: CoreSet,
    decisions: Vec<Seq>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Finding {
    seq: Seq,
    title: String,
    detail: String,
    pr: u32,
    direction: DefectDirection,
    cores: CoreSet,
    decisions: Vec<Seq>,
    guidance: CoreSet,
}

#[derive(Clone, Copy, Debug)]
pub struct PendingFinding<'a> {
    pub entry: &'static Entry,
    pub cores: CoreSet,
    pub decisions: &'a [Seq],
}

#[derive(Clone, Copy, Debug)]
pub struct FindingView<'a> {
    pub id: DefectId,
    pub entry: Option<&'static Entry>,
    pub seq: Seq,
    pub title: &'a str,
    pub detail: &'a str,
    pub pr: u32,
    pub direction: DefectDirection,
    pub cores: CoreSet,
    pub decisions: &'a [Seq],
}

#[derive(Clone, Copy, Debug)]
pub struct Guidance<'a> {
    pub finding: FindingView<'a>,
    pub cores: CoreSet,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefectState {
    registry: &'static [Entry],
    watched: Vec<EventKind>,
    kinds: BTreeMap<Seq, EventKind>,
    fixes: u32,
    unrecorded: BTreeMap<DefectId, Matches>,
    findings: BTreeMap<DefectId, Finding>,
    order: Vec<DefectId>,
    answered: BTreeSet<DefectId>,
    pending_resets: BTreeMap<Seq, CoreSet>,
    excluded: BTreeSet<Seq>,
}

impl Default for DefectState {
    fn default() -> Self {
        Self::new(KNOWN_DEFECTS)
    }
}

impl DefectState {
    pub fn new(registry: &'static [Entry]) -> Self {
        assert!(
            is_ordered(registry),
            "defect ids must be strictly increasing"
        );
        let mut watched = vec![EventKind::Failure];
        for clause in registry.iter().flat_map(|entry| entry.clauses) {
            if !watched.contains(&clause.cause_kind) {
                watched.push(clause.cause_kind);
            }
        }
        Self {
            registry,
            watched,
            kinds: BTreeMap::new(),
            fixes: 0,
            unrecorded: BTreeMap::new(),
            findings: BTreeMap::new(),
            order: Vec::new(),
            answered: BTreeSet::new(),
            pending_resets: BTreeMap::new(),
            excluded: BTreeSet::new(),
        }
    }

    pub fn fold(&mut self, event: &Event) {
        let kind = event.kind();
        if self.watched.contains(&kind) {
            self.kinds.insert(event.seq, kind);
        }
        match &event.payload {
            Payload::SessionStart(start) => self.fixes = start.build.fixes,
            Payload::ConfigLoaded(loaded) => self.fixes = loaded.build.fixes,
            Payload::TunerDecision(decision) => self.fold_decision(event, decision),
            Payload::DefectFound(found) => self.fold_found(event.seq, found),
            Payload::DefectAnswered(answered) => self.fold_answer(event.seq, answered),
            Payload::CommandReset(reset) => self.fold_reset(event, *reset),
            Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
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
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_) => {}
        }
    }

    fn fold_decision(&mut self, event: &Event, decision: &TunerDecision) {
        for entry in self.registry {
            if self.fixes >= entry.id.get() {
                continue;
            }
            let mut matched = false;
            for (cause, cause_kind) in Self::matched_causes(&self.kinds, entry, event, decision) {
                matched = true;
                if cause_kind == EventKind::Failure {
                    self.excluded.insert(cause);
                }
            }
            if matched && !self.findings.contains_key(&entry.id) {
                let matches = self.unrecorded.entry(entry.id).or_default();
                matches.cores.insert(decision.core);
                matches.decisions.push(event.seq);
            }
        }
    }

    fn matched_causes<'a>(
        kinds: &'a BTreeMap<Seq, EventKind>,
        entry: &'a Entry,
        event: &'a Event,
        decision: &'a TunerDecision,
    ) -> impl Iterator<Item = (Seq, EventKind)> + 'a {
        event.cause.iter().filter_map(move |&cause| {
            let cause_kind = *kinds.get(&cause)?;
            entry
                .clauses
                .iter()
                .any(|clause| {
                    clause.decision_kind == EventKind::TunerDecision
                        && clause.decision == decision.decision
                        && clause.cause_kind == cause_kind
                })
                .then_some((cause, cause_kind))
        })
    }

    fn fold_found(&mut self, seq: Seq, found: &DefectFound) {
        let Some(id) = DefectId::new(found.id) else {
            return;
        };
        if self.findings.contains_key(&id) {
            return;
        }
        self.unrecorded.remove(&id);
        let guidance = match found.direction {
            DefectDirection::TooCautious => found.cores,
            DefectDirection::TooAggressive => CoreSet::default(),
        };
        self.findings.insert(
            id,
            Finding {
                seq,
                title: found.title.clone(),
                detail: found.detail.clone(),
                pr: found.pr,
                direction: found.direction,
                cores: found.cores,
                decisions: found.decisions.clone(),
                guidance,
            },
        );
        self.order.push(id);
    }

    fn fold_answer(&mut self, seq: Seq, answered: &DefectAnswered) {
        if let Some(id) = DefectId::new(answered.id) {
            self.answered.insert(id);
        }
        if answered.answer == DefectAnswer::Yes && !answered.cores.is_empty() {
            self.pending_resets.insert(seq, answered.cores);
        }
    }

    fn fold_reset(&mut self, event: &Event, reset: CommandReset) {
        match reset.target {
            ResetTarget::All => {
                for finding in self.findings.values_mut() {
                    finding.guidance = CoreSet::default();
                }
            }
            ResetTarget::Core(core) => {
                for finding in self.findings.values_mut() {
                    finding.guidance.remove(core);
                }
                for cause in &event.cause {
                    if let Some(cores) = self.pending_resets.get_mut(cause) {
                        cores.remove(core);
                        if cores.is_empty() {
                            self.pending_resets.remove(cause);
                        }
                    }
                }
            }
        }
    }

    pub fn new_findings(&self) -> impl Iterator<Item = PendingFinding<'_>> {
        self.registry.iter().filter_map(|entry| {
            let matches = self.unrecorded.get(&entry.id)?;
            Some(PendingFinding {
                entry,
                cores: matches.cores,
                decisions: &matches.decisions,
            })
        })
    }

    pub fn unanswered(&self) -> impl Iterator<Item = FindingView<'_>> {
        self.order
            .iter()
            .filter(|id| !self.answered.contains(id))
            .map(|&id| self.view(id))
    }

    pub fn finding_seq(&self, id: DefectId) -> Option<Seq> {
        self.findings.get(&id).map(|finding| finding.seq)
    }

    pub fn missing_resets(&self) -> impl Iterator<Item = (Seq, CoreId)> {
        self.pending_resets
            .iter()
            .flat_map(|(&answer, cores)| cores.iter().map(move |core| (answer, core)))
    }

    pub fn excludes_failure(&self, failure: Seq) -> bool {
        self.excluded.contains(&failure)
    }

    pub fn excluded_failures(&self) -> impl Iterator<Item = Seq> {
        self.excluded.iter().copied()
    }

    pub fn reset_guidance(&self) -> impl Iterator<Item = Guidance<'_>> {
        self.order.iter().filter_map(|&id| {
            let cores = self.findings[&id].guidance;
            (!cores.is_empty()).then(|| Guidance {
                finding: self.view(id),
                cores,
            })
        })
    }

    fn view(&self, id: DefectId) -> FindingView<'_> {
        let finding = &self.findings[&id];
        let entry = self
            .registry
            .binary_search_by_key(&id.get(), |entry| entry.id.get())
            .ok()
            .map(|at| &self.registry[at]);
        FindingView {
            id,
            entry,
            seq: finding.seq,
            title: &finding.title,
            detail: &finding.detail,
            pr: finding.pr,
            direction: finding.direction,
            cores: finding.cores,
            decisions: &finding.decisions,
        }
    }
}
