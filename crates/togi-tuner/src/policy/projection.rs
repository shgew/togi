//! What status and watch show of the policy, and the `tuner` key of `state.json`: the committed edges and
//! caps, the applied profile and its hold, the events and vetoes, the qualifications and the latest night.

use serde::{Deserialize, Serialize};
use togi_journal::{
    Activity, Binding, Budget, Classification, DayEventId, DayStep, Edge, ExposureClass, NightId,
    PlannedTask, QualificationPurpose, QualificationState, ReproductionState, Resolution, Seq,
    Slope, TaskId, TaskStatus, UnavailableBackend, VetoState,
};
use togi_machine::{BootId, CcdId, CcdProfile, CoreSet, TargetOffset};

use super::State;
use super::ledger::saturating_u32;
use super::qualify::QualStatus;
use crate::day::{Freeze, QuestionState};
use crate::night::Cursor;

/// A committed cap S and what produced it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CapProjection {
    pub offset: TargetOffset,
    pub binding: Binding,
    pub slope: Slope,
    pub adequate: Vec<ExposureClass>,
    pub uncovered: Vec<ExposureClass>,
    /// The `ccd.cap` event.
    pub witness: Seq,
}

/// One CCD's committed edge U, cap S, applied day offset D and the distance between them.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CcdProjection {
    pub ccd: CcdId,
    pub cores: CoreSet,
    /// `None` until a `ccd.edge` was committed; absence is not offset zero.
    pub edge: Option<Edge>,
    pub cap: Option<CapProjection>,
    /// D: CO 0 before any `day.applied`.
    pub day: TargetOffset,
    /// Counts from D down to S; zero when D is already at or past S. `None` without a cap.
    pub remaining: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EventProjection {
    pub event: DayEventId,
    pub classification: Classification,
    pub resolution: Resolution,
    pub reproduction: ReproductionState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VetoProjection {
    pub event: DayEventId,
    pub state: VetoState,
}

/// Where a qualification record stands.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationStage {
    Start,
    Progress,
    Complete,
    Failed,
}

/// A qualification record and where it stands.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QualificationProjection {
    pub profile: CcdProfile,
    pub purpose: QualificationPurpose,
    pub state: QualificationStage,
    /// Whether it was made under the current firmware, epoch and content.
    pub current: bool,
}

/// The applied-hour hold at the current D.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HoldProjection {
    pub profile: CcdProfile,
    pub eligible_s: u64,
    pub required_s: u64,
    /// The `day.hold` event.
    pub witness: Seq,
}

/// Whether the committed firmware context still matches the one D was applied under.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FirmwareValidity {
    Valid,
    /// The context changed at `since` and D has not been reset yet.
    Invalidated {
        since: Seq,
    },
    /// No firmware context was reported yet.
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DayProjection {
    pub hold: Option<HoldProjection>,
    pub last_step: Option<(Seq, DayStep)>,
    pub freeze: bool,
    pub firmware: FirmwareValidity,
    /// Boots whose crash question is still pending.
    pub pending_questions: Vec<BootId>,
}

/// Where one CCD's mapping descent stands.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MappingProjection {
    pub ccd: CcdId,
    /// The offset in progress.
    pub cursor: Option<TargetOffset>,
    /// The first failing coarse step and the bracket offset in progress.
    pub bracket: Option<(TargetOffset, TargetOffset)>,
}

/// The open qualification's progress.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QualificationProgress {
    pub profile: CcdProfile,
    pub outstanding: Vec<ExposureClass>,
    pub witnessed: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReproductionProjection {
    pub source: DayEventId,
    pub state: ReproductionState,
    pub consumed_s: u64,
    pub remaining: Vec<ExposureClass>,
}

/// The latest plan of the latest night.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NightProjection {
    pub night: NightId,
    /// When the plan that opened the night was committed.
    pub started: jiff::Timestamp,
    /// The event that closed the night (a later boot's `day.boot_start`, the operator's signal stop or a
    /// session reset); `None` while it is open.
    pub closed_by: Option<Seq>,
    pub budgets: Vec<Budget>,
    pub tasks: Vec<PlannedTask>,
    /// The running task and its activity. A filler task runs cap probes (mapping) and exact repeats
    /// (qualification): its activity is its latest trial's, unknown before its first trial.
    pub current: Option<(TaskId, Option<Activity>)>,
    pub mapping: Vec<MappingProjection>,
    pub qualification: Option<QualificationProgress>,
    pub reproductions: Vec<ReproductionProjection>,
    /// The backends the night runs no cell on and the events that excluded each, as its plan records them.
    pub unavailable: Vec<UnavailableBackend>,
    /// The durable filler cursor: how many filler trials ran across every night, which rotates the next
    /// filler cell.
    pub filler: u32,
}

/// The `tuner` key of `state.json` and what `status` and `watch` read.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Projection {
    pub applied: Option<CcdProfile>,
    pub ccds: Vec<CcdProjection>,
    pub day: DayProjection,
    pub events: Vec<EventProjection>,
    pub vetoes: Vec<VetoProjection>,
    pub qualifications: Vec<QualificationProjection>,
    pub night: Option<NightProjection>,
    /// A conclusive failure at all-zero ended the run.
    pub stopped: bool,
}

const fn activity_of(queue: togi_journal::Queue) -> Option<Activity> {
    match queue {
        togi_journal::Queue::Reproduction => Some(Activity::Reproduction),
        togi_journal::Queue::Qualification | togi_journal::Queue::Requalification => {
            Some(Activity::Qualification)
        }
        togi_journal::Queue::Mapping => Some(Activity::Mapping),
        togi_journal::Queue::Control => Some(Activity::Control),
        togi_journal::Queue::Filler => None,
    }
}

fn offset(value: i64) -> TargetOffset {
    TargetOffset::clamped(value)
}

impl State {
    /// The committed state as status shows it. Pure: it reads only what the journal committed, plus the
    /// mapping cursors and the open qualification the same journal derives.
    #[must_use]
    pub fn project(&self) -> Projection {
        let mut events: Vec<_> = self.days.events.values().collect();
        events.sort_by_key(|book| book.first);
        let mut vetoes: Vec<_> = self.days.vetoes.values().collect();
        vetoes.sort_by_key(|commit| commit.seq);
        let applied = self
            .days
            .history
            .as_ref()
            .map(|history| history.current().profile.clone());
        Projection {
            ccds: self.ccd_projections(applied.as_ref()),
            applied,
            day: self.day_projection(),
            events: events
                .into_iter()
                .map(|book| {
                    let event = &book.latest.payload;
                    EventProjection {
                        event: event.event.clone(),
                        classification: event.classification,
                        resolution: event.resolution,
                        reproduction: event.reproduction,
                    }
                })
                .collect(),
            vetoes: vetoes
                .into_iter()
                .map(|commit| VetoProjection {
                    event: commit.payload.event.clone(),
                    state: commit.payload.state,
                })
                .collect(),
            qualifications: self
                .qualification_records()
                .iter()
                .map(|record| QualificationProjection {
                    profile: record.start.payload.profile.ccd.clone(),
                    purpose: record.start.payload.purpose,
                    state: match record.latest.payload.state {
                        QualificationState::Start { .. } => QualificationStage::Start,
                        QualificationState::Progress { .. } => QualificationStage::Progress,
                        QualificationState::Complete { .. } => QualificationStage::Complete,
                        QualificationState::Failed { .. } => QualificationStage::Failed,
                    },
                    current: self.is_current_record(record),
                })
                .collect(),
            night: self.night_projection(),
            stopped: self.stop.is_some(),
        }
    }

    fn ccd_projections(&self, applied: Option<&CcdProfile>) -> Vec<CcdProjection> {
        let Some(topology) = self.machine.topology.as_ref() else {
            return Vec::new();
        };
        topology
            .ccds()
            .iter()
            .map(|&(ccd, cores)| {
                let day = applied
                    .and_then(|profile| profile.get(ccd))
                    .unwrap_or(TargetOffset::ZERO);
                let cap = self.nights.caps.get(&ccd);
                CcdProjection {
                    ccd,
                    cores,
                    edge: self
                        .nights
                        .edges
                        .get(&ccd)
                        .map(|edge| edge.payload.edge.clone()),
                    remaining: cap.map(|cap| {
                        saturating_u32(
                            day.get()
                                .saturating_sub(cap.payload.new.get())
                                .max(0)
                                .unsigned_abs(),
                        )
                    }),
                    cap: cap.map(|cap| CapProjection {
                        offset: cap.payload.new,
                        binding: cap.payload.binding.clone(),
                        slope: cap.payload.slope,
                        adequate: cap.payload.adequate.clone(),
                        uncovered: cap.payload.uncovered.clone(),
                        witness: cap.seq,
                    }),
                    day,
                }
            })
            .collect()
    }

    fn day_projection(&self) -> DayProjection {
        let firmware = match (
            &self.machine.firmware,
            &self.machine.anchor,
            self.machine.changed_at,
        ) {
            (None, _, _) | (Some(_), None, _) => FirmwareValidity::Unknown,
            (Some(current), Some(anchor), Some(since)) if current != anchor => {
                FirmwareValidity::Invalidated { since }
            }
            (Some(current), Some(anchor), None) if current != anchor => FirmwareValidity::Unknown,
            (Some(_), Some(_), _) => FirmwareValidity::Valid,
        };
        let pending_questions = self
            .day_view()
            .map(|view| {
                view.events
                    .iter()
                    .filter(|event| event.question == QuestionState::Pending)
                    .filter_map(|event| match &event.id {
                        crate::day::EventId::Boot(boot) => Some(boot.clone()),
                        crate::day::EventId::Unobserved(_)
                        | crate::day::EventId::Requalification(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        DayProjection {
            hold: self.days.holds.last().and_then(|hold| {
                let profile = self.days.history.as_ref()?.current().profile.clone();
                Some(HoldProjection {
                    profile,
                    eligible_s: hold.payload.eligible_s,
                    required_s: self.params.hold.hold_secs,
                    witness: hold.seq,
                })
            }),
            last_step: self
                .days
                .steps
                .values()
                .next_back()
                .map(|step| (step.seq, step.payload.clone())),
            freeze: self.days.freeze == Freeze::On,
            firmware,
            pending_questions,
        }
    }

    fn night_projection(&self) -> Option<NightProjection> {
        let (night, book) = self.nights.plans.iter().next_back()?;
        let plan = &book.latest.payload;
        let known = self.known_contents(None);
        let mapping = self
            .with_layout(|layout| self.mapping_plan(layout, &known))
            .map(|plan| {
                self.ccds()
                    .into_iter()
                    .filter_map(|ccd| {
                        let mapping = plan.mapped.board.ccd(ccd)?;
                        let (cursor, bracket) = match mapping.cursor() {
                            Cursor::Coarse { offset: at } => (Some(offset(at)), None),
                            Cursor::Sweep { next } => (Some(offset(next)), None),
                            Cursor::Bracket { failed, next } => {
                                (Some(offset(next)), Some((offset(failed), offset(next))))
                            }
                        };
                        Some(MappingProjection {
                            ccd,
                            cursor,
                            bracket,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let qualification = self.with_layout(|_| Ok(())).ok().and_then(|()| {
            self.qualification_records()
                .iter()
                .rev()
                .find_map(|record| {
                    if !self.is_current_record(record) {
                        return None;
                    }
                    match &*self.qualification_status(record).ok()? {
                        QualStatus::Open {
                            witnesses,
                            outstanding,
                            ..
                        } => Some(QualificationProgress {
                            profile: record.start.payload.profile.ccd.clone(),
                            outstanding: outstanding.clone(),
                            witnessed: witnesses.len(),
                        }),
                        QualStatus::Stale
                        | QualStatus::Complete { .. }
                        | QualStatus::Failed { .. } => None,
                    }
                })
        });
        let mut reproductions: Vec<_> = self.nights.reproductions.iter().collect();
        reproductions.sort_by_key(|commit| commit.seq);
        let mut latest: Vec<ReproductionProjection> = Vec::new();
        for commit in reproductions {
            let entry = ReproductionProjection {
                source: commit.payload.source.clone(),
                state: commit.payload.state,
                consumed_s: commit.payload.consumed_s,
                remaining: commit.payload.remaining.clone(),
            };
            match latest.iter_mut().find(|known| known.source == entry.source) {
                Some(known) => *known = entry,
                None => latest.push(entry),
            }
        }
        Some(NightProjection {
            night: *night,
            started: book.first.time,
            closed_by: book.closed_by,
            budgets: plan.budgets.clone(),
            current: plan
                .tasks
                .iter()
                .find(|task| task.status == TaskStatus::Running)
                .map(|task| {
                    let latest = self
                        .trials
                        .by_task
                        .get(&task.task)
                        .and_then(|trials| trials.last())
                        .and_then(|&at| self.trials.all.get(at))
                        .and_then(|trial| trial.intent.activity);
                    (task.task, activity_of(task.task.queue).or(latest))
                }),
            tasks: plan.tasks.clone(),
            mapping,
            qualification,
            reproductions: latest,
            unavailable: plan.unavailable.clone(),
            filler: self.trials.filler_cursor(),
        })
    }
}
