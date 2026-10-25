//! Exact-profile qualification and re-qualification: the frozen matrix a night commits to, what
//! satisfies it and what it concludes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};
use togi_journal::{
    CcdVector, CellEvidence, CellWitness, ExposureClass, NightQualification, Outcome,
    QualificationPurpose, QualificationState, Queue, Seq, TaskId, Tested, WorkloadContent,
};
use togi_machine::{BiosContext, CcdId, CcdProfile, ContentId, Digest, WorkloadId};

use super::State;
use super::books::Committed;
use super::decision::{Decision, PolicyPayload};
use super::ledger::{Sig, is_adequate};
use super::night_query::NotReady;
use super::plan::workloads_of;
use crate::EVIDENCE_EPOCH;
use crate::day::{CoreProfile, propose, vetoes};
use crate::night::{Cell, Layout, Matrix, Reuse, Source, qualification_cells};

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
        profile: CcdProfile,
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
    pub(super) fn ccd_vector(&self, profile: &CcdProfile) -> Result<CcdVector, NotReady> {
        Ok(CcdVector {
            ccd: profile.clone(),
            cores: profile.expand(&self.machine.cores)?,
        })
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
    fn applied_before(&self, seq: Seq) -> Option<&CcdProfile> {
        let history = self.days.history.as_ref()?;
        history
            .iter()
            .rev()
            .find(|application| application.at < seq)
            .or_else(|| history.iter().next())
            .map(|application| &application.profile)
    }

    /// The matrix the qualification of `profile` freezes at `ordinal`, with reuse witnesses drawn from
    /// the exposures journaled at or before `cutoff`, and the evidence seq of each reused cell.
    /// The matrix of `profile` as the journal up to `cutoff` derives it, partial chains included.
    fn matrix_at(
        &self,
        profile: &CcdProfile,
        ordinal: u32,
        cutoff: Seq,
        applied: Option<&CcdProfile>,
    ) -> Result<(Matrix, HashMap<Cell, Seq>), NotReady> {
        self.layout_at(Some(cutoff), |layout| {
            self.matrix_in(layout, profile, ordinal, cutoff, applied)
        })
    }

    fn matrix_in(
        &self,
        layout: &Layout<'_>,
        profile: &CcdProfile,
        ordinal: u32,
        cutoff: Seq,
        applied: Option<&CcdProfile>,
    ) -> Result<(Matrix, HashMap<Cell, Seq>), NotReady> {
        let changed: BTreeSet<CcdId> = profile
            .iter()
            .filter(|&(ccd, offset)| applied.is_none_or(|applied| applied.get(ccd) != Some(offset)))
            .map(|(ccd, _)| ccd)
            .collect();
        let candidates = qualification_cells(layout, profile, ordinal, &Reuse::AllFresh)?;
        let mut witnesses: HashMap<Cell, Seq> = HashMap::new();
        for planned in &candidates.cells {
            let cell = planned.cell;
            let Some(ccd) = self.single_ccd(&cell) else {
                continue;
            };
            if changed.contains(&ccd) {
                continue;
            }
            let Some(offset) = profile.get(ccd) else {
                continue;
            };
            let sig = Sig::of_cell(&cell);
            let Some(witness) = self
                .current_groups()
                .filter_map(|group| group.qual_witness.get(&(ccd, offset, sig.clone())))
                .copied()
                .filter(|&seq| seq <= cutoff)
                .min()
            else {
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

    /// The one CCD a cell's loaded cores sit on.
    fn single_ccd(&self, cell: &Cell) -> Option<CcdId> {
        let mut ccds = cell.loaded.iter().filter_map(|core| {
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
        let bytes = serde_json::to_vec(matrix)?;
        let hash: [u8; 32] = Sha256::digest(bytes).into();
        Ok(Digest::from_bytes(hash))
    }

    /// The frozen matrix of a record, `None` when what the journal derives no longer hashes to what the
    /// record committed.
    fn frozen_matrix(&self, record: &QualRecord) -> Result<Option<FrozenMatrix>, NotReady> {
        let start = &record.start;
        let cutoff = start.causes.iter().copied().max().unwrap_or(start.seq);
        let (matrix, reused) = self.matrix_at(
            &start.payload.profile.ccd,
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
        let profile = &record.start.payload.profile.ccd;
        for exposure in task
            .get(entry.read..)
            .unwrap_or_default()
            .iter()
            .filter_map(|&index| self.ledger.records.get(index))
            .filter(|exposure| {
                self.is_current(&exposure.observed)
                    && exposure.observed.tested == Tested::Exact {}
                    && &exposure.observed.profile.ccd == profile
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
        profile: &'a CcdProfile,
    ) -> impl Iterator<Item = &'a QualRecord> {
        records.iter().filter(move |record| {
            &record.start.payload.profile.ccd == profile && self.is_current_record(record)
        })
    }

    /// The profiles with a complete, current qualification no older than the re-qualification period at
    /// `now`: the ones a promotion may apply.
    pub(super) fn qualified_profiles(&self, now: jiff::Timestamp) -> BTreeSet<CcdProfile> {
        self.qualification_records()
            .iter()
            .filter(|record| {
                matches!(
                    record.latest.payload.state,
                    QualificationState::Complete { .. }
                ) && self.is_current_record(record)
                    && !Self::elapsed(record, now)
            })
            .map(|record| record.start.payload.profile.ccd.clone())
            .collect()
    }

    fn elapsed(record: &QualRecord, now: jiff::Timestamp) -> bool {
        let age = now
            .as_second()
            .saturating_sub(record.latest.time.as_second());
        age >= REQUALIFICATION_DAYS * 24 * 3600
    }

    /// Whether a current qualification of `profile` ended in failure.
    fn failed_current(&self, records: &[QualRecord], profile: &CcdProfile) -> bool {
        self.current_records(records, profile).any(|record| {
            matches!(
                record.latest.payload.state,
                QualificationState::Failed { .. }
            )
        })
    }

    /// The next admissible promotion proposal: D moved toward the committed cap S, unless an active veto
    /// forbids it. A vetoed proposal is held, not replaced by another CCD or step, so there is nothing
    /// to qualify until a cap or a veto changes.
    pub(super) fn proposal(&self) -> Result<Option<CcdProfile>, NotReady> {
        let Some(history) = self.days.history.as_ref() else {
            return Ok(None);
        };
        let caps = self.committed_caps()?;
        let Some(proposal) = propose(&history.current().profile, &caps)? else {
            return Ok(None);
        };
        let vetoes = vetoes(&self.day_view()?.events);
        if !vetoes.is_empty() {
            let core = CoreProfile::of_ccd(&proposal.profile, &self.machine.cores)?;
            if vetoes.iter().any(|veto| veto.profile.forbids(&core)) {
                return Ok(None);
            }
        }
        Ok(Some(proposal.profile))
    }

    /// What the two qualification queues want: the next promotion proposal's qualification, and the
    /// re-qualification of D.
    pub(super) fn qualification_work(
        &self,
        now: jiff::Timestamp,
    ) -> Result<(QualWork, QualWork, Option<crate::night::RequalificationDue>), NotReady> {
        let records = self.qualification_records();
        let Some(history) = self.days.history.as_ref() else {
            return Ok((QualWork::None, QualWork::None, None));
        };
        let applied = &history.current().profile;
        // After a firmware change the profile D held before is re-qualified exactly, in the new context,
        // before it is restored; until that qualification stands or fails the ordinary proposal waits.
        let retained = match Self::retained_profile(history) {
            Some(profile) => {
                self.work_for(&records, profile, QualificationPurpose::Firmware, now)?
            }
            None => QualWork::None,
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
    fn retained_profile(history: &crate::day::DayHistory) -> Option<CcdProfile> {
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
        profile: CcdProfile,
        purpose: QualificationPurpose,
        now: jiff::Timestamp,
    ) -> Result<QualWork, NotReady> {
        if self.failed_current(records, &profile) {
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
        profile: &CcdProfile,
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
