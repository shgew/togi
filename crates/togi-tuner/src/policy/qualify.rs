//! Exact-profile qualification and re-qualification: the frozen matrix a night commits to, what
//! satisfies it and what it concludes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};
use togi_journal::{
    Activity, CcdVector, CellEvidence, CellWitness, ExposureClass, NightQualification, Outcome,
    QualificationPurpose, QualificationState, Queue, Seq, TaskId, Tested, WorkloadContent,
};
use togi_machine::{BiosContext, CcdId, ContentId, CoreSet, Digest, TargetOffset, WorkloadId};

use super::State;
use super::books::Committed;
use super::decision::{Decision, PolicyPayload};
use super::ledger::{Sig, is_adequate};
use super::night_query::NotReady;
use super::plan::workloads_of;
use crate::day::{CoreProfile, awaiting, next_proposal, shallower, vetoes};
use crate::night::{Cell, Layout, Matrix, Reuse, Source, qualification_cells};
use crate::{EVIDENCE_EPOCH, UnitProfile};

/// How long a complete qualification lasts before re-qualification is due.
pub(super) const REQUALIFICATION_DAYS: i64 = 30;

/// A qualification record: its start commitment and the latest commitment of the same identity.
#[derive(Clone, Debug)]
pub(super) struct QualRecord {
    /// The durable ordinal: how many qualifications started before it.
    pub(super) index: u32,
    pub(super) start: Committed<NightQualification>,
    pub(super) latest: Committed<NightQualification>,
}

impl QualRecord {
    pub(super) const fn queue(&self) -> Queue {
        match self.start.payload.purpose {
            QualificationPurpose::Requalification => Queue::Requalification,
            QualificationPurpose::NextStep | QualificationPurpose::Firmware => Queue::Qualification,
        }
    }

    pub(super) const fn task(&self) -> TaskId {
        TaskId {
            queue: self.queue(),
            ordinal: self.index,
        }
    }

    fn contents(&self) -> BTreeMap<WorkloadId, ContentId> {
        self.start
            .payload
            .contents
            .iter()
            .map(|entry| (entry.workload.runs(), entry.content.clone()))
            .collect()
    }
}

/// What the exposure ledger says of a record against its frozen matrix.
#[derive(Clone, Debug)]
pub(super) enum QualStatus {
    /// The frozen matrix no longer matches what the journal derives: no conclusion can be drawn.
    Stale,
    Open {
        witnesses: Vec<CellWitness>,
        outstanding: Vec<ExposureClass>,
        remaining: Vec<Cell>,
    },
    Complete {
        witnesses: Vec<CellWitness>,
    },
    Failed {
        failure: Seq,
        witnesses: Vec<CellWitness>,
        outstanding: Vec<ExposureClass>,
    },
}

/// What the qualification and re-qualification queues have to offer the scheduler.
#[derive(Clone, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "commitments are built, journaled and dropped at once, never stored in bulk"
)]
pub(super) enum QualWork {
    /// Nothing is wanted.
    None,
    /// A matrix is wanted but not yet frozen.
    NeedStart {
        profile: UnitProfile,
        purpose: QualificationPurpose,
    },
    /// A frozen matrix has cells outstanding.
    Open {
        record: QualRecord,
        remaining: Vec<Cell>,
    },
}

/// The qualification records grouped from the first `folded` commitments, the last of them at `last`.
#[derive(Clone, Debug, Default)]
pub(super) struct RecordCache {
    folded: usize,
    last: Option<Seq>,
    records: Arc<Vec<QualRecord>>,
}

/// Each record's status derivation, by durable ordinal.
pub(super) type StatusCache = BTreeMap<u32, StatusEntry>;

/// A record's status as derived so far. The frozen matrix reads only journal facts at or before the
/// record's cutoff, under the session's cores and the evidence context; the record's task exposures only
/// append, so each is read once.
#[derive(Clone, Debug)]
pub(super) struct StatusEntry {
    context: StatusContext,
    /// The frozen matrix, `None` when it no longer hashes to what the record committed.
    frozen: Option<FrozenMatrix>,
    /// How many of the task's ledger records have been read.
    read: usize,
    /// The first current exact failure of the profile.
    failure: Option<Seq>,
    /// The first current adequate exact pass of the profile for each class.
    passes: HashMap<Sig, Seq>,
    status: Arc<QualStatus>,
    /// The record's latest commitment the status was last found to agree with: no commitment is owed
    /// until one of the two moves.
    settled: Option<Seq>,
}

/// What a record's derivation depends on beyond the journal facts it reads.
#[derive(Clone, Debug)]
struct StatusContext {
    start: Seq,
    session: Option<togi_journal::SessionId>,
    firmware: Option<BiosContext>,
    contents: BTreeMap<WorkloadId, ContentId>,
}

impl StatusContext {
    fn fits(&self, state: &State, record: &QualRecord) -> bool {
        self.start == record.start.seq
            && self.session == state.machine.session
            && self.firmware == state.machine.firmware
            && self.contents == state.machine.contents
    }

    fn of(state: &State, record: &QualRecord) -> Self {
        Self {
            start: record.start.seq,
            session: state.machine.session,
            firmware: state.machine.firmware.clone(),
            contents: state.machine.contents.clone(),
        }
    }
}

/// Adds one qualification commitment to the records: a start opens a record, any other state becomes the
/// latest of the newest record of the same profile and matrix.
fn group_commit(records: &mut Vec<QualRecord>, commit: &Committed<NightQualification>) {
    if matches!(commit.payload.state, QualificationState::Start { .. }) {
        records.push(QualRecord {
            index: super::ledger::saturating_u32(u64::try_from(records.len()).unwrap_or(u64::MAX)),
            start: commit.clone(),
            latest: commit.clone(),
        });
    } else if let Some(record) = records.iter_mut().rev().find(|record| {
        record.start.payload.profile == commit.payload.profile
            && record.start.payload.matrix == commit.payload.matrix
    }) {
        record.latest = commit.clone();
    }
}

impl State {
    pub(super) fn ccd_vector(&self, profile: &UnitProfile) -> Result<CcdVector, NotReady> {
        Ok(CcdVector::expand(
            profile.to_ccd().ok_or(NotReady::NonCoarse)?,
            &self.machine.cores,
        )?)
    }

    /// Every qualification commitment grouped into records, in order of start. Grouped incrementally:
    /// commitments only append, so each is grouped once.
    pub(super) fn qualification_records(&self) -> Arc<Vec<QualRecord>> {
        let commits = &self.nights.qualifications;
        let mut cache = self.caches.records.lock();
        let folded = cache.folded;
        let consistent = folded <= commits.len()
            && folded
                .checked_sub(1)
                .and_then(|at| commits.get(at))
                .map(|commit| commit.seq)
                == cache.last;
        if !consistent {
            *cache = RecordCache::default();
        }
        let folded = cache.folded;
        if let Some(fresh) = commits.get(folded..).filter(|fresh| !fresh.is_empty()) {
            let records = Arc::make_mut(&mut cache.records);
            for commit in fresh {
                group_commit(records, commit);
            }
            cache.folded = commits.len();
            cache.last = commits.last().map(|commit| commit.seq);
        }
        Arc::clone(&cache.records)
    }

    /// Whether the record was made under the current firmware, epoch and workload content.
    pub(super) fn is_current_record(&self, record: &QualRecord) -> bool {
        let qualification = &record.start.payload;
        self.machine.firmware.as_ref() == Some(&qualification.firmware)
            && qualification.epoch == EVIDENCE_EPOCH
            && qualification.contents.iter().all(|entry| {
                self.machine.contents.get(&entry.workload.runs()) == Some(&entry.content)
            })
    }

    /// The profile D applied just before `seq`.
    fn applied_before(&self, seq: Seq) -> Option<&UnitProfile> {
        let history = self.days.history.as_ref()?;
        history
            .iter()
            .rev()
            .find(|application| application.at < seq)
            .or_else(|| history.iter().next())
            .map(|application| &application.profile)
    }

    /// The matrix the qualification of `profile` for `purpose` freezes at `ordinal`, as the journal up to
    /// `cutoff` derives it, partial chains included, and the evidence seq of each reused cell.
    fn matrix_at(
        &self,
        profile: &UnitProfile,
        purpose: QualificationPurpose,
        ordinal: u32,
        cutoff: Seq,
        applied: Option<&UnitProfile>,
    ) -> Result<(Matrix, HashMap<Cell, Seq>), NotReady> {
        self.layout_at(Some(cutoff), |layout| {
            self.matrix_in(layout, profile, purpose, ordinal, cutoff, applied)
        })
    }

    /// A monthly re-qualification runs every cell afresh. Otherwise a cell on one CCD whose offset `profile`
    /// leaves at D's reuses the exposure a completed certificate at that offset cites for it, unless a
    /// later failure at that offset or deeper contradicts it (`docs/spec/workloads.md`, Reuse).
    fn matrix_in(
        &self,
        layout: &Layout<'_>,
        profile: &UnitProfile,
        purpose: QualificationPurpose,
        ordinal: u32,
        cutoff: Seq,
        applied: Option<&UnitProfile>,
    ) -> Result<(Matrix, HashMap<Cell, Seq>), NotReady> {
        if purpose == QualificationPurpose::Requalification {
            let matrix = qualification_cells(layout, profile, ordinal, &Reuse::AllFresh)?;
            return Ok((matrix, HashMap::new()));
        }
        let units = crate::Units::for_profile(layout.topology, profile);
        let changed: BTreeSet<CcdId> = self
            .machine
            .cores
            .iter()
            .filter_map(|info| {
                let unit = units.unit_of(info.core)?;
                applied
                    .is_none_or(|applied| applied.get(unit) != profile.get(unit))
                    .then_some(info.ccd)
            })
            .collect();
        let certified = self.certified_witnesses(cutoff);
        let candidates = qualification_cells(layout, profile, ordinal, &Reuse::AllFresh)?;
        let mut witnesses: HashMap<Cell, Seq> = HashMap::new();
        for planned in &candidates.cells {
            let cell = planned.cell;
            let Some(ccd) = self.single_ccd(cell.loaded) else {
                continue;
            };
            if changed.contains(&ccd) {
                continue;
            }
            let Some(offset) = profile.get(crate::UnitId::Ccd(ccd)) else {
                continue;
            };
            let Some(&witness) = certified.get(&(ccd, offset, Sig::of_cell(&cell))) else {
                continue;
            };
            let contradicted = self.current_groups().any(|group| {
                group.scoped_failures.get(&ccd).is_some_and(|failures| {
                    failures
                        .iter()
                        .any(|&(seq, failed)| seq > witness && seq <= cutoff && failed >= offset)
                })
            });
            if !contradicted {
                witnesses.insert(cell, witness);
            }
        }
        let set: HashSet<Cell> = witnesses.keys().copied().collect();
        let matrix = qualification_cells(
            layout,
            profile,
            ordinal,
            &Reuse::Unchanged {
                changed: &changed,
                witnesses: &set,
            },
        )?;
        Ok((matrix, witnesses))
    }

    /// What the current certificates completed by `cutoff` witnessed: for each cell on one CCD, at the
    /// offset its certificate qualified there, the earliest exposure that satisfied it.
    fn certified_witnesses(&self, cutoff: Seq) -> HashMap<(CcdId, TargetOffset, Sig), Seq> {
        let mut certified: HashMap<(CcdId, TargetOffset, Sig), Seq> = HashMap::new();
        for record in self
            .qualification_records()
            .iter()
            .filter(|record| record.latest.seq <= cutoff && self.is_current_record(record))
        {
            let QualificationState::Complete { witnesses } = &record.latest.payload.state else {
                continue;
            };
            for witness in witnesses {
                let Some(ccd) = self.single_ccd(witness.cell.cores) else {
                    continue;
                };
                let Some(offset) = record.start.payload.profile.ccd().get(ccd) else {
                    continue;
                };
                let (CellEvidence::Fresh { exposure } | CellEvidence::Reused { exposure }) =
                    witness.evidence;
                certified
                    .entry((ccd, offset, Sig::of_class(&witness.cell)))
                    .and_modify(|earliest| *earliest = (*earliest).min(exposure))
                    .or_insert(exposure);
            }
        }
        certified
    }

    /// The one CCD `cores` sit on.
    fn single_ccd(&self, cores: CoreSet) -> Option<CcdId> {
        let mut ccds = cores.iter().filter_map(|core| {
            self.machine
                .cores
                .iter()
                .find(|info| info.core == core)
                .map(|info| info.ccd)
        });
        let first = ccds.next()?;
        ccds.all(|ccd| ccd == first).then_some(first)
    }

    fn matrix_digest(matrix: &Matrix) -> Result<Digest, NotReady> {
        // The released qualification hash includes the historical CCD profile representation.
        #[derive(serde::Serialize)]
        struct JournalMatrix<'a> {
            profile: togi_machine::CcdProfile,
            cells: &'a [crate::night::PlannedCell],
            uncovered: &'a [crate::night::Uncovered],
        }
        let bytes = serde_json::to_vec(&JournalMatrix {
            profile: matrix.profile.to_ccd().ok_or(NotReady::NonCoarse)?,
            cells: &matrix.cells,
            uncovered: &matrix.uncovered,
        })?;
        let hash: [u8; 32] = Sha256::digest(bytes).into();
        Ok(Digest::from_bytes(hash))
    }

    /// The frozen matrix of a record, `None` when what the journal derives no longer hashes to what the
    /// record committed.
    fn frozen_matrix(&self, record: &QualRecord) -> Result<Option<FrozenMatrix>, NotReady> {
        let start = &record.start;
        let cutoff = start.causes.iter().copied().max().unwrap_or(start.seq);
        let (matrix, reused) = self.matrix_at(
            &UnitProfile::of_ccd(start.payload.profile.ccd()),
            start.payload.purpose,
            record.index,
            cutoff,
            self.applied_before(start.seq),
        )?;
        Ok((Self::matrix_digest(&matrix)? == start.payload.matrix).then_some((matrix, reused)))
    }

    /// What the exposures say of `record`. The frozen matrix is derived once per evidence context, and
    /// each of the task's exposures is read once.
    pub(super) fn qualification_status(
        &self,
        record: &QualRecord,
    ) -> Result<Arc<QualStatus>, NotReady> {
        let entry = self.status_entry(record)?;
        let status = Arc::clone(&entry.status);
        self.caches.statuses.lock().insert(record.index, entry);
        Ok(status)
    }

    /// The qualification commitment `record` owes: its status where that differs from the record's
    /// latest, an open one's progress only when `progress` asks for it.
    pub(super) fn qualification_owed(
        &self,
        record: &QualRecord,
        progress: bool,
    ) -> Result<Option<NightQualification>, NotReady> {
        let mut entry = self.status_entry(record)?;
        let owed = if entry.settled == Some(record.latest.seq) {
            None
        } else {
            let owed = Self::qualification_commit(record, &entry.status, progress);
            let compared = progress || !matches!(*entry.status, QualStatus::Open { .. });
            if owed.is_none() && compared {
                entry.settled = Some(record.latest.seq);
            }
            owed
        };
        self.caches.statuses.lock().insert(record.index, entry);
        Ok(owed)
    }

    /// The cached derivation of `record`, taken out of the cache and brought up to date; the caller puts
    /// it back.
    fn status_entry(&self, record: &QualRecord) -> Result<StatusEntry, NotReady> {
        let task: &[usize] = self
            .ledger
            .by_task
            .get(&record.task())
            .map_or(&[], Vec::as_slice);
        let cached = self
            .caches
            .statuses
            .lock()
            .remove(&record.index)
            .filter(|entry| entry.context.fits(self, record));
        let mut entry = match cached {
            Some(entry) if entry.read == task.len() => return Ok(entry),
            Some(entry) => entry,
            None => StatusEntry {
                context: StatusContext::of(self, record),
                frozen: self.frozen_matrix(record)?,
                read: 0,
                failure: None,
                passes: HashMap::new(),
                status: Arc::new(QualStatus::Stale),
                settled: None,
            },
        };
        let profile = record.start.payload.profile.ccd();
        for exposure in task
            .get(entry.read..)
            .unwrap_or_default()
            .iter()
            .filter_map(|&index| self.ledger.records.get(index))
            .filter(|exposure| {
                self.is_current(&exposure.observed)
                    && exposure.observed.tested == Tested::Exact {}
                    && exposure.observed.profile.ccd() == profile
            })
        {
            match exposure.observed.outcome {
                Outcome::Failure => {
                    entry.failure.get_or_insert(exposure.seq);
                }
                Outcome::Pass => {
                    if is_adequate(&exposure.observed, &self.machine.cores) {
                        entry
                            .passes
                            .entry(Sig::of_class(&exposure.observed.class))
                            .or_insert(exposure.seq);
                    }
                }
                Outcome::Inconclusive => {}
            }
        }
        entry.read = task.len();
        entry.status = Arc::new(match &entry.frozen {
            Some(frozen) => Self::status_of(record, frozen, entry.failure, &entry.passes)?,
            None => QualStatus::Stale,
        });
        entry.settled = None;
        Ok(entry)
    }

    /// The status of `record` against its frozen matrix, given its first failure and the first adequate
    /// pass of each class.
    fn status_of(
        record: &QualRecord,
        (matrix, reused): &FrozenMatrix,
        failure: Option<Seq>,
        passes: &HashMap<Sig, Seq>,
    ) -> Result<QualStatus, NotReady> {
        let contents = record.contents();
        let mut witnesses = Vec::new();
        let mut outstanding = Vec::new();
        let mut remaining = Vec::new();
        for planned in &matrix.cells {
            let cell = planned.cell;
            let Some(class) = Self::class_of(&cell, &contents) else {
                return Err(NotReady::UnknownContent(workloads_of(cell.workloads)[0]));
            };
            match planned.source {
                Source::Reused => {
                    if let Some(&exposure) = reused.get(&cell) {
                        witnesses.push(CellWitness {
                            cell: class,
                            evidence: CellEvidence::Reused { exposure },
                        });
                    }
                }
                Source::Fresh => {
                    if let Some(&exposure) = passes.get(&Sig::of_cell(&cell)) {
                        witnesses.push(CellWitness {
                            cell: class,
                            evidence: CellEvidence::Fresh { exposure },
                        });
                    } else {
                        outstanding.push(class);
                        remaining.push(cell);
                    }
                }
            }
        }
        Ok(match failure {
            Some(failure) => QualStatus::Failed {
                failure,
                witnesses,
                outstanding,
            },
            None if remaining.is_empty() => QualStatus::Complete { witnesses },
            None => QualStatus::Open {
                witnesses,
                outstanding,
                remaining,
            },
        })
    }

    /// The qualification commitment `status` calls for, when it differs from the record's latest.
    fn qualification_commit(
        record: &QualRecord,
        status: &QualStatus,
        progress: bool,
    ) -> Option<NightQualification> {
        let latest = &record.latest.payload.state;
        let unchanged = match status {
            QualStatus::Stale => return None,
            QualStatus::Open {
                witnesses,
                outstanding,
                ..
            } => {
                if !progress {
                    return None;
                }
                matches!(latest, QualificationState::Progress { witnesses: w, outstanding: o }
                    if w == witnesses && o == outstanding)
            }
            QualStatus::Complete { witnesses } => {
                matches!(latest, QualificationState::Complete { witnesses: w } if w == witnesses)
            }
            QualStatus::Failed {
                failure,
                witnesses,
                outstanding,
            } => {
                matches!(latest, QualificationState::Failed { failure: f, witnesses: w, outstanding: o }
                if f == failure && w == witnesses && o == outstanding)
            }
        };
        if unchanged {
            return None;
        }
        let state = match status {
            QualStatus::Stale => return None,
            QualStatus::Open {
                witnesses,
                outstanding,
                ..
            } => QualificationState::Progress {
                witnesses: witnesses.clone(),
                outstanding: outstanding.clone(),
            },
            QualStatus::Complete { witnesses } => QualificationState::Complete {
                witnesses: witnesses.clone(),
            },
            QualStatus::Failed {
                failure,
                witnesses,
                outstanding,
            } => QualificationState::Failed {
                failure: *failure,
                witnesses: witnesses.clone(),
                outstanding: outstanding.clone(),
            },
        };
        Some(NightQualification {
            state,
            ..record.start.payload.clone()
        })
    }

    /// The latest committed state of a current record for `profile` that satisfies `pick`.
    fn current_records<'a>(
        &'a self,
        records: &'a [QualRecord],
        profile: &'a UnitProfile,
    ) -> impl Iterator<Item = &'a QualRecord> {
        records.iter().filter(move |record| {
            profile.matches_ccd(record.start.payload.profile.ccd())
                && self.is_current_record(record)
        })
    }

    /// The profiles with a complete, current qualification no older than the re-qualification period at
    /// `now`: the ones a promotion may apply.
    pub(super) fn qualified_profiles(&self, now: jiff::Timestamp) -> BTreeSet<UnitProfile> {
        self.qualification_records()
            .iter()
            .filter(|record| {
                matches!(
                    record.latest.payload.state,
                    QualificationState::Complete { .. }
                ) && self.is_current_record(record)
                    && !Self::elapsed(record, now)
            })
            .map(|record| UnitProfile::of_ccd(record.start.payload.profile.ccd()))
            .collect()
    }

    fn elapsed(record: &QualRecord, now: jiff::Timestamp) -> bool {
        let age = now
            .as_second()
            .saturating_sub(record.latest.time.as_second());
        age >= REQUALIFICATION_DAYS * 24 * 3600
    }

    /// Whether a current qualification of `profile` ended in failure.
    fn failed_current(&self, records: &[QualRecord], profile: &UnitProfile) -> bool {
        self.current_records(records, profile).any(|record| {
            matches!(
                record.latest.payload.state,
                QualificationState::Failed { .. }
            )
        })
    }

    /// The witnesses that block `profile` from qualification, ascending, when a qualification of it as a
    /// proposal other than D failed conclusively in this session and firmware: its exact failures, and
    /// the failed commitment when it was written. Raw failures alone block, so an interrupted failed
    /// commitment still does; neither content, night nor restart lifts the block
    /// (`docs/spec/tuner.md`, Failed next-step qualification).
    pub(super) fn qualification_blocked(&self, profile: &UnitProfile) -> Option<Vec<Seq>> {
        let firmware = self.machine.firmware.as_ref()?;
        let session = self.machine.session?;
        let mut blocked = Vec::new();
        for record in self.qualification_records().iter().filter(|record| {
            record.queue() == Queue::Qualification
                && profile.matches_ccd(record.start.payload.profile.ccd())
                && &record.start.payload.firmware == firmware
                && record.start.payload.epoch == EVIDENCE_EPOCH
        }) {
            let failures = self
                .ledger
                .by_task
                .get(&record.task())
                .into_iter()
                .flatten();
            let start = blocked.len();
            blocked.extend(
                failures
                    .filter_map(|&index| self.ledger.records.get(index))
                    .filter(|exposure| {
                        let observed = &exposure.observed;
                        observed.outcome == Outcome::Failure
                            && observed.activity == Activity::Qualification
                            && observed.tested == Tested::Exact {}
                            && profile.matches_ccd(observed.profile.ccd())
                            && &observed.firmware == firmware
                            && observed.epoch == EVIDENCE_EPOCH
                            && observed.source.session == session
                    })
                    .map(|exposure| exposure.seq),
            );
            if let QualificationState::Failed { failure, .. } = &record.latest.payload.state
                && blocked
                    .get(start..)
                    .is_some_and(|own| own.contains(failure))
            {
                blocked.push(record.latest.seq);
            }
        }
        blocked.sort_unstable();
        blocked.dedup();
        (!blocked.is_empty()).then_some(blocked)
    }

    /// The next admissible promotion proposal: the restoration a reclassified step back owes, else D moved
    /// toward the committed cap S, unless an active veto
    /// forbids it. A vetoed proposal is held, not replaced by another CCD or step, so there is nothing
    /// to qualify until a cap or a veto changes.
    pub(super) fn proposal(&self) -> Result<Option<UnitProfile>, NotReady> {
        let Some(history) = self.days.history.as_ref() else {
            return Ok(None);
        };
        let caps = self.committed_caps()?;
        let view = self.day_view()?;
        let Some(proposal) = next_proposal(history, &view.events, &caps)? else {
            return Ok(None);
        };
        let vetoes = vetoes(&view.events);
        if !vetoes.is_empty() {
            let core = CoreProfile::of_units(&proposal.profile, &self.machine.cores)?;
            if vetoes.iter().any(|veto| veto.profile.forbids(&core)) {
                return Ok(None);
            }
        }
        Ok(Some(proposal.profile))
    }

    /// The retreat profile CO 0 is holding for a required step back, as the day would restore it under
    /// the committed caps; `None` when CO 0 holds nothing or the restoration would be CO 0 itself.
    fn fallback_target(&self) -> Result<Option<UnitProfile>, NotReady> {
        let view = self.day_view()?;
        let Some(statuses) = self.statuses(&view)? else {
            return Ok(None);
        };
        let Some((_, wanted)) = awaiting(&statuses)? else {
            return Ok(None);
        };
        let target = shallower(&wanted, &self.committed_caps()?)?;
        Ok((!target.is_zero()).then_some(target))
    }

    /// What the two qualification queues want: an outstanding fallback retreat's profile first, then the
    /// next promotion proposal's qualification, and the re-qualification of D.
    pub(super) fn qualification_work(
        &self,
        now: jiff::Timestamp,
    ) -> Result<(QualWork, QualWork, Option<crate::night::RequalificationDue>), NotReady> {
        let records = self.qualification_records();
        let Some(history) = self.days.history.as_ref() else {
            return Ok((QualWork::None, QualWork::None, None));
        };
        let applied = &history.current().profile;
        // A required step back whose profile lacked qualification holds CO 0 until that profile
        // qualifies, and its unresolved event blocks every promotion meanwhile: it goes first.
        let fallback = match self.fallback_target()? {
            Some(profile) if profile != *applied => {
                self.work_for(&records, profile, QualificationPurpose::NextStep, now)?
            }
            Some(_) | None => QualWork::None,
        };
        // After a firmware change the profile D held before is re-qualified exactly, in the new context,
        // before it is restored; until that qualification stands or fails the ordinary proposal waits.
        let retained = match Self::retained_profile(history) {
            Some(profile) if matches!(fallback, QualWork::None) => {
                self.work_for(&records, profile, QualificationPurpose::Firmware, now)?
            }
            Some(_) | None => fallback,
        };
        let next = if matches!(retained, QualWork::None) {
            match self.proposal()? {
                Some(proposal) => {
                    self.work_for(&records, proposal, QualificationPurpose::NextStep, now)?
                }
                None => QualWork::None,
            }
        } else {
            retained
        };
        let (requal, due) = if applied.is_zero() {
            (QualWork::None, None)
        } else {
            let due = if self.current_records(&records, applied).any(|record| {
                matches!(
                    record.latest.payload.state,
                    QualificationState::Complete { .. }
                )
            }) {
                crate::night::RequalificationDue::Elapsed
            } else {
                crate::night::RequalificationDue::Invalidated
            };
            (
                self.work_for(
                    &records,
                    applied.clone(),
                    QualificationPurpose::Requalification,
                    now,
                )?,
                Some(due),
            )
        };
        Ok((next, requal, due))
    }

    /// The nonzero profile applied just before the latest firmware zero, while D has not returned to it.
    fn retained_profile(history: &crate::day::DayHistory) -> Option<UnitProfile> {
        let applications: Vec<_> = history.iter().collect();
        let zeroed = applications.iter().rposition(|application| {
            application.kind == crate::day::ApplicationKind::FirmwareZero
        })?;
        let before = &applications.get(zeroed.checked_sub(1)?)?.profile;
        (!before.is_zero() && *before != history.current().profile).then(|| before.clone())
    }

    fn work_for(
        &self,
        records: &[QualRecord],
        profile: UnitProfile,
        purpose: QualificationPurpose,
        now: jiff::Timestamp,
    ) -> Result<QualWork, NotReady> {
        // A failed proposal stays failed for the session and firmware; a failed re-qualification of D is
        // the day's to answer.
        let failed = match purpose {
            QualificationPurpose::NextStep | QualificationPurpose::Firmware => {
                self.qualification_blocked(&profile).is_some()
            }
            QualificationPurpose::Requalification => self.failed_current(records, &profile),
        };
        if failed {
            return Ok(QualWork::None);
        }
        let mut open: Option<QualWork> = None;
        for record in self.current_records(records, &profile) {
            match &record.latest.payload.state {
                QualificationState::Complete { .. } if !Self::elapsed(record, now) => {
                    return Ok(QualWork::None);
                }
                QualificationState::Complete { .. } | QualificationState::Failed { .. } => {}
                QualificationState::Start { .. } | QualificationState::Progress { .. } => {
                    if let QualStatus::Open { remaining, .. } =
                        &*self.qualification_status(record)?
                    {
                        open = Some(QualWork::Open {
                            record: record.clone(),
                            remaining: remaining.clone(),
                        });
                    }
                }
            }
        }
        Ok(open.unwrap_or(QualWork::NeedStart { profile, purpose }))
    }

    /// The commitment that freezes the matrix of `profile` and starts its qualification.
    pub(super) fn start_qualification(
        &self,
        profile: &UnitProfile,
        purpose: QualificationPurpose,
        contents: &BTreeMap<WorkloadId, ContentId>,
    ) -> Result<Decision, NotReady> {
        let firmware = self.machine.firmware.clone().ok_or(NotReady::NoFirmware)?;
        let ordinal = super::ledger::saturating_u32(
            u64::try_from(self.qualification_records().len()).unwrap_or(u64::MAX),
        );
        let cutoff = self.last.ok_or(NotReady::NoSession)?;
        let (matrix, _) = self.matrix_at(
            profile,
            purpose,
            ordinal,
            cutoff,
            self.days
                .history
                .as_ref()
                .map(|history| &history.current().profile),
        )?;
        let mut workloads: BTreeSet<WorkloadId> = BTreeSet::new();
        for planned in &matrix.cells {
            workloads.extend(
                workloads_of(planned.cell.workloads)
                    .into_iter()
                    .map(WorkloadId::runs),
            );
        }
        let entries = workloads
            .into_iter()
            .map(|workload| {
                contents
                    .get(&workload)
                    .map(|content| WorkloadContent {
                        workload,
                        content: content.clone(),
                    })
                    .ok_or(NotReady::UnknownContent(workload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let known: BTreeMap<WorkloadId, ContentId> = entries
            .iter()
            .map(|entry| (entry.workload, entry.content.clone()))
            .collect();
        let outstanding = matrix
            .fresh()
            .filter_map(|cell| Self::class_of(cell, &known))
            .collect();
        Ok(Decision::new(
            PolicyPayload::NightQualification(NightQualification {
                profile: self.ccd_vector(profile)?,
                firmware,
                epoch: EVIDENCE_EPOCH,
                contents: entries,
                matrix: Self::matrix_digest(&matrix)?,
                purpose,
                state: QualificationState::Start { outstanding },
            }),
            vec![cutoff],
        ))
    }
}

/// A frozen matrix with the commitment each reused cell came from.
type FrozenMatrix = (Matrix, HashMap<Cell, Seq>);
