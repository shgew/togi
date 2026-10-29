use std::collections::{BTreeMap, BTreeSet};

use togi_journal::{
    DefectDirection, DefectFound, DefectId, Event, EventKind, FixStamp, Payload, Seq,
};
use togi_machine::{CcdId, CoreSet};

use crate::registry::{Entry, KNOWN_DEFECTS, is_ordered};

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct Matches {
    cores: CoreSet,
    decisions: Vec<Seq>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct Finding {
    seq: Seq,
    title: String,
    detail: String,
    pr: u32,
    direction: DefectDirection,
    cores: CoreSet,
    decisions: Vec<Seq>,
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

#[cfg(test)]
#[derive(Clone, Debug, Default)]
struct Visits(std::cell::Cell<u64>);

#[cfg(test)]
impl PartialEq for Visits {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[cfg(test)]
impl Eq for Visits {}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DefectState {
    #[serde(skip, default = "known_defects")]
    registry: &'static [Entry],
    watched: Vec<EventKind>,
    kinds: BTreeMap<Seq, EventKind>,
    fixes: FixStamp,
    unrecorded: BTreeMap<DefectId, Matches>,
    findings: BTreeMap<DefectId, Finding>,
    order: Vec<DefectId>,
    excluded: BTreeSet<Seq>,
    ccds: Vec<(CcdId, CoreSet)>,
    #[cfg(test)]
    #[serde(skip)]
    visits: Visits,
}

fn known_defects() -> &'static [Entry] {
    KNOWN_DEFECTS
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
        let mut watched = Vec::new();
        for clause in registry.iter().flat_map(|entry| entry.clauses) {
            if !watched.contains(&clause.cause_kind) {
                watched.push(clause.cause_kind);
            }
        }
        Self {
            registry,
            watched,
            kinds: BTreeMap::new(),
            fixes: FixStamp::none(),
            unrecorded: BTreeMap::new(),
            findings: BTreeMap::new(),
            order: Vec::new(),
            excluded: BTreeSet::new(),
            ccds: Vec::new(),
            #[cfg(test)]
            visits: Visits::default(),
        }
    }

    #[cfg(test)]
    fn visit(&self) {
        self.visits.0.set(self.visits.0.get() + 1);
    }

    #[cfg(not(test))]
    #[expect(
        clippy::unused_self,
        reason = "the test build counts query visits here"
    )]
    fn visit(&self) {}

    #[cfg(test)]
    pub(crate) fn take_visits(&self) -> u64 {
        self.visits.0.replace(0)
    }

    #[cfg(test)]
    pub(crate) fn retained_kinds(&self) -> usize {
        self.kinds.len()
    }

    pub fn fold(&mut self, event: &Event) {
        let kind = event.kind();
        if self.watched.contains(&kind) {
            self.kinds.insert(event.seq, kind);
        }
        match &event.payload {
            Payload::SessionStart(start) => {
                self.fixes = start.build.fixes;
                self.ccds.clear();
                for info in &start.cores {
                    match self.ccds.iter_mut().find(|(ccd, _)| *ccd == info.ccd) {
                        Some((_, cores)) => {
                            cores.insert(info.core);
                        }
                        None => self
                            .ccds
                            .push((info.ccd, std::iter::once(info.core).collect())),
                    }
                }
            }
            Payload::ConfigLoaded(loaded) => self.fixes = loaded.build.fixes,
            Payload::CcdEdge(edge) => self.fold_decision(event, Some(edge.ccd)),
            Payload::CcdCap(cap) => self.fold_decision(event, Some(cap.ccd)),
            Payload::NightPlan(_)
            | Payload::NightQualification(_)
            | Payload::NightReproduction(_) => self.fold_decision(event, None),
            Payload::DefectFound(found) => self.fold_found(event.seq, found),
            // Older per-core answers and resets are archived history: no Ruleset 13 finding is resolved by them.
            Payload::DefectAnswered(_)
            | Payload::CommandReset(_)
            | Payload::SessionContext(_)
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
            | Payload::TaskStart(_)
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
            | Payload::DayStep(_)
            | Payload::DayApplied(_)
            | Payload::DayEvent(_)
            | Payload::DayVeto(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayInterval(_)
            | Payload::DayPause(_)
            | Payload::DayImported(_)
            | Payload::DayUncleanEnd(_)
            | Payload::TunerDecision(_)
            | Payload::StateRebuilt(_) => {}
        }
    }

    fn fold_decision(&mut self, event: &Event, ccd: Option<CcdId>) {
        let cores: CoreSet = ccd
            .and_then(|ccd| self.ccds.iter().find(|(known, _)| *known == ccd))
            .map(|(_, cores)| *cores)
            .unwrap_or_default();
        for entry in self.registry {
            if self.fixes.fixes(entry.id) {
                continue;
            }
            let mut matched = false;
            for (cause, cause_kind) in Self::matched_causes(&self.kinds, entry, event) {
                matched = true;
                if cause_kind == EventKind::Failure {
                    self.excluded.insert(cause);
                }
            }
            if matched && !self.findings.contains_key(&entry.id) {
                let matches = self.unrecorded.entry(entry.id).or_default();
                matches.cores.extend(cores);
                matches.decisions.push(event.seq);
            }
        }
    }

    fn matched_causes<'a>(
        kinds: &'a BTreeMap<Seq, EventKind>,
        entry: &'a Entry,
        event: &'a Event,
    ) -> impl Iterator<Item = (Seq, EventKind)> + 'a {
        event.cause.iter().filter_map(move |&cause| {
            let cause_kind = *kinds.get(&cause)?;
            entry
                .clauses
                .iter()
                .any(|clause| {
                    clause.decision_kind == event.kind() && clause.cause_kind == cause_kind
                })
                .then_some((cause, cause_kind))
        })
    }

    fn fold_found(&mut self, seq: Seq, found: &DefectFound) {
        let id = found.id;
        if self.findings.contains_key(&id) {
            return;
        }
        self.unrecorded.remove(&id);
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
            },
        );
        self.order.push(id);
    }

    pub fn new_findings(&self) -> impl Iterator<Item = PendingFinding<'_>> {
        self.registry.iter().filter_map(|entry| {
            self.visit();
            let matches = self.unrecorded.get(&entry.id)?;
            Some(PendingFinding {
                entry,
                cores: matches.cores,
                decisions: &matches.decisions,
            })
        })
    }

    /// Every recorded finding, in journal order. None is ever resolved inside a session: only `togi reset --all`,
    /// which starts a fresh one, clears them.
    pub fn findings(&self) -> impl Iterator<Item = FindingView<'_>> {
        self.order.iter().map(|&id| {
            self.visit();
            self.view(id)
        })
    }

    pub fn finding_seq(&self, id: DefectId) -> Option<Seq> {
        self.visit();
        self.findings.get(&id).map(|finding| finding.seq)
    }

    pub fn excludes_failure(&self, failure: Seq) -> bool {
        self.visit();
        self.excluded.contains(&failure)
    }

    pub fn excluded_failures(&self) -> impl Iterator<Item = Seq> {
        self.excluded.iter().map(|&failure| {
            self.visit();
            failure
        })
    }

    fn view(&self, id: DefectId) -> FindingView<'_> {
        self.visit();
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
