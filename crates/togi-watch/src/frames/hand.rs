//! Hand-built frames for tests: the model's empty snapshot with a session, cores, a trial and a policy projection
//! set by hand, and builders for the projections the panels show.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use ratatui::buffer::Buffer;
use togi_journal::{
    Activity, Binding, Budget, Classification, DayEventId, DayStep, DeadEndCondition, DecisionId,
    Edge, ExposureClass, Finite, LowerEndpoint, NightId, Outcome, Placement, PlannedTask,
    QualificationPurpose, Queue, ReproductionState, Resolution, ShutdownReason, Slope, StepCauses,
    StepKind, TaskId, TaskStatus, Tested, UncoveredClass, UncoveredReason,
};
use togi_machine::{BootId, CcdId, CcdProfile, ObservedOffset, Regime, ResetKind, TargetOffset};
use togi_tuner::policy::{
    CapProjection, CcdProjection, DayProjection, EventProjection, FirmwareValidity, HoldProjection,
    MappingProjection, NightProjection, Projection, QualificationProgress, QualificationProjection,
    QualificationStage, ReproductionProjection,
};

use super::testing::{FIXED_NOW, draw_journal, main_screen};
use crate::day_totals::DayTotals;
use crate::history::{Entry, LogEntry, What};
use crate::model::{
    CoreView, DeadEndView, Model, RecoveryView, SessionView, Snapshot, StopView, TrialEndView,
    TrialView,
};
use crate::screen::{Drawn, Journal, Screen};
use crate::tests::fixtures;
use crate::tests::kit::{expanded, off, seq, set};
use crate::words::{Tag, Tone};

/// A snapshot of an empty model with a session, cores, a trial in flight and a policy set by hand.
pub(crate) struct Hand {
    model: Model,
    pub(crate) session: SessionView,
    pub(crate) cores: Vec<CoreView>,
    pub(crate) trial: Option<TrialView>,
    pub(crate) last: Option<TrialEndView>,
    pub(crate) recover: Option<RecoveryView>,
    pub(crate) stopped: Option<StopView>,
    pub(crate) dead_end: Option<DeadEndView>,
    pub(crate) history: Vec<Entry>,
    pub(crate) log: Vec<LogEntry>,
    pub(crate) last_failure: Option<Timestamp>,
    pub(crate) policy: Projection,
    pub(crate) totals: DayTotals,
    /// When the frame draws: 40 seconds after the fixed time unless a test moves it.
    pub(crate) now: Timestamp,
    /// The day steps a `day.applied` carried out.
    pub(crate) applied_steps: std::collections::BTreeSet<DecisionId>,
}

impl Hand {
    pub(crate) fn new(cores: Vec<CoreView>, policy: Projection) -> Self {
        Self {
            model: Model::new(),
            session: SessionView {
                start: FIXED_NOW,
                activity: None,
            },
            cores,
            trial: None,
            last: None,
            recover: None,
            stopped: None,
            dead_end: None,
            history: Vec::new(),
            log: Vec::new(),
            last_failure: None,
            policy,
            totals: DayTotals::default(),
            now: FIXED_NOW + SignedDuration::from_secs(40),
            applied_steps: std::collections::BTreeSet::new(),
        }
    }

    pub(crate) fn snapshot(&self) -> Snapshot<'_> {
        Snapshot {
            session: Some(&self.session),
            cores: &self.cores,
            trial: self.trial.as_ref(),
            last: self.last.as_ref(),
            recover: self.recover.as_ref(),
            stopped: self.stopped.as_ref(),
            dead_end: self.dead_end.as_ref(),
            history: &self.history,
            log: &self.log,
            log_total: self.log.len(),
            last_failure: self.last_failure,
            policy: &self.policy,
            day_totals: &self.totals,
            applied_steps: &self.applied_steps,
            ..self.model.snapshot()
        }
    }

    pub(crate) fn journal(&self) -> Journal<'_> {
        let snapshot = self.snapshot();
        Journal::Ready {
            snapshot,
            log: snapshot.log(),
        }
    }

    pub(crate) fn draw(&self, screen: &Screen, now: Timestamp) -> (Buffer, Drawn) {
        draw_journal(&self.journal(), screen, now, &TimeZone::UTC)
    }

    /// The main view at `width` x `height`, plain, at [`Self::now`].
    pub(crate) fn main(&self, width: u16, height: u16) -> (Buffer, Drawn) {
        self.draw(&main_screen(width, height), self.now)
    }
}

/// A core of CCD `ccd` with its readback at `applied` (`None`: no readback) and the profile at `profile`.
pub(crate) fn core_row(id: u8, ccd: u8, applied: Option<i64>, loaded: bool) -> CoreView {
    CoreView {
        applied: applied.map(ObservedOffset),
        profile: off(applied.unwrap_or(0).clamp(-50, 0)),
        loaded,
        ..fixtures::core_view(id, ccd)
    }
}

pub(crate) fn ccd(id: u8) -> CcdId {
    CcdId::try_from(id).expect("a CCD id")
}

/// A running trial of `regime` that started at the fixed time and lasts 10 minutes.
pub(crate) fn running_trial(regime: Regime, cores: &[u8]) -> TrialView {
    TrialView {
        cores: set(cores),
        started: Some(FIXED_NOW),
        duration: Duration::from_secs(600),
        ..fixtures::trial_view(regime)
    }
}

/// Mapping task `ordinal` of the night, testing CCD `ccd` alone at `offset`.
pub(crate) fn mapping_trial(ordinal: u32, ccd_id: u8, offset: i64, cores: &[u8]) -> TrialView {
    TrialView {
        task: Some(TaskId {
            queue: Queue::Mapping,
            ordinal,
        }),
        tested: Some(Tested::Isolated {
            ccd: ccd(ccd_id),
            offset: off(offset),
        }),
        activity: Some(Activity::Mapping),
        ..running_trial(Regime::R1, cores)
    }
}

/// The projection of a machine with no committed edge, cap, day or night.
pub(crate) fn bare(ccds: Vec<CcdProjection>) -> Projection {
    Projection {
        applied: None,
        ccds,
        day: DayProjection {
            hold: None,
            last_step: None,
            freeze: false,
            firmware: FirmwareValidity::Unknown,
            pending_questions: Vec::new(),
        },
        events: Vec::new(),
        vetoes: Vec::new(),
        qualifications: Vec::new(),
        night: None,
        stopped: false,
    }
}

/// CCD `id` over `cores` at day offset `day`, with no edge and no cap.
pub(crate) fn ccd_row(id: u8, cores: &[u8], day: i64) -> CcdProjection {
    CcdProjection {
        ccd: ccd(id),
        cores: set(cores),
        edge: None,
        cap: None,
        evidence: None,
        day: off(day),
        remaining: None,
    }
}

pub(crate) fn observed(offset: i64) -> Edge {
    Edge::Observed {
        offset: off(offset),
        witness: seq(7),
    }
}

pub(crate) fn censored(limit: i64) -> Edge {
    Edge::Censored {
        limit: off(limit),
        witnesses: vec![seq(7), seq(9)],
    }
}

/// An exposure class of `regime` over `cores`.
pub(crate) fn class(regime: Regime, cores: &[u8]) -> ExposureClass {
    ExposureClass {
        regime,
        workloads: Vec::new(),
        cores: set(cores),
        position: None,
        duration_s: 600,
        placement: Placement::Isolated,
    }
}

/// A cap at `offset` that `binding` limits, backed by `covered` classes with `uncovered` still missing.
pub(crate) fn cap(
    offset: i64,
    binding: Binding,
    slope: Slope,
    covered: usize,
    uncovered: usize,
) -> CapProjection {
    CapProjection {
        offset: off(offset),
        binding,
        slope,
        adequate: (0..covered).map(|_| class(Regime::R1, &[0])).collect(),
        uncovered: (0..uncovered)
            .map(|_| UncoveredClass {
                class: class(Regime::R7, &[0, 1]),
                reason: UncoveredReason::MappingInadequate,
            })
            .collect(),
        witness: seq(11),
    }
}

pub(crate) fn fallback() -> Slope {
    Slope::Fallback {
        beta: Finite::new(0.15).expect("a finite slope"),
    }
}

pub(crate) fn measured() -> Slope {
    Slope::Measured {
        lower: LowerEndpoint::Unbounded {},
        historical_min: None,
    }
}

pub(crate) fn rate_bound() -> Binding {
    Binding::Rate {
        class: class(Regime::R1, &[0]),
    }
}

/// `ccd` with its cap, edge and remaining distance set.
pub(crate) fn with_cap(
    mut ccd: CcdProjection,
    cap: CapProjection,
    edge: Option<Edge>,
    remaining: u32,
) -> CcdProjection {
    ccd.cap = Some(cap);
    ccd.edge = edge;
    ccd.remaining = Some(remaining);
    ccd
}

/// A hold of `eligible_s` of `required_s` seconds at the applied profile.
pub(crate) fn hold(profile: &[(u8, i64)], eligible_s: u64, required_s: u64) -> HoldProjection {
    HoldProjection {
        profile: profile_of(profile),
        eligible_s,
        required_s,
        witness: seq(13),
    }
}

pub(crate) fn profile_of(profile: &[(u8, i64)]) -> CcdProfile {
    CcdProfile::try_from(
        profile
            .iter()
            .map(|&(id, value)| (ccd(id), TargetOffset::clamped(value)))
            .collect::<BTreeMap<_, _>>(),
    )
    .expect("a CCD profile")
}

/// An open night that started at the fixed time and has observed half an hour, with mapping underway on `current`.
pub(crate) fn night(current: Option<(Queue, u32, Activity)>) -> NightProjection {
    NightProjection {
        night: NightId(seq(5)),
        started: FIXED_NOW,
        closed_by: None,
        budgets: vec![
            Budget {
                queue: Queue::Mapping,
                share_pct: 10,
                consumed_s: 1200,
            },
            Budget {
                queue: Queue::Qualification,
                share_pct: 75,
                consumed_s: 600,
            },
            Budget {
                queue: Queue::Control,
                share_pct: 5,
                consumed_s: 0,
            },
            Budget {
                queue: Queue::Filler,
                share_pct: 0,
                consumed_s: 0,
            },
        ],
        tasks: vec![
            PlannedTask {
                task: TaskId {
                    queue: Queue::Mapping,
                    ordinal: 1,
                },
                status: TaskStatus::Complete,
            },
            PlannedTask {
                task: TaskId {
                    queue: Queue::Mapping,
                    ordinal: 2,
                },
                status: TaskStatus::Running,
            },
        ],
        current: current
            .map(|(queue, ordinal, activity)| (TaskId { queue, ordinal }, Some(activity))),
        mapping: Vec::new(),
        qualification: None,
        reproductions: Vec::new(),
        unavailable: Vec::new(),
        filler: 0,
    }
}

/// A day step of `kind` over CCD 0 at CO 0 with no causes.
pub(crate) fn step(kind: StepKind) -> DayStep {
    let vector = expanded(profile_of(&[(0, 0)]));
    DayStep {
        decision: DecisionId(1),
        cold_boot: BootId::from("boot-1".to_owned()),
        old: vector.clone(),
        proposed: vector,
        step: kind,
        predecessor: None,
        hours: Vec::new(),
        hold_s: 0,
        causes: StepCauses::default(),
        reason: String::new(),
    }
}

/// A trial that ended in a pass.
pub(crate) fn ended_trial(regime: Regime) -> TrialEndView {
    TrialEndView {
        id: "t0".to_owned().into(),
        at: FIXED_NOW,
        regime: Some(regime),
        cores: set(&[0]),
        outcome: Outcome::Pass,
        signal: None,
        core: None,
        duration: Duration::from_secs(600),
        planned: Some(Duration::from_secs(600)),
        last_sample: None,
        stalled: None,
        tctl_max_c: None,
        voltage_v: None,
    }
}

/// The sixteen cores of two CCDs, each at the readback `applied` gives its id.
pub(crate) fn sixteen(applied: impl Fn(u8) -> Option<i64>) -> Vec<CoreView> {
    (0..16)
        .map(|id| core_row(id, id / 8, applied(id), false))
        .collect()
}

fn load(cores: &mut [CoreView], ids: std::ops::Range<u8>) {
    for core in cores.iter_mut().filter(|core| ids.contains(&core.id.get())) {
        core.loaded = true;
    }
}

/// A policy with two CCDs: CCD 0 capped and observed to fail, CCD 1 capped with a censored edge.
fn two_ccds() -> Projection {
    let first = with_cap(
        ccd_row(0, &[0, 1, 2, 3, 4, 5, 6, 7], -10),
        cap(-26, rate_bound(), fallback(), 3, 1),
        Some(observed(-31)),
        16,
    );
    let second = with_cap(
        ccd_row(1, &[8, 9, 10, 11, 12, 13, 14, 15], -8),
        cap(-15, rate_bound(), measured(), 4, 0),
        Some(censored(-50)),
        7,
    );
    let mut projection = bare(vec![first, second]);
    projection.applied = Some(profile_of(&[(0, -10), (1, -8)]));
    projection.day.hold = Some(hold(&[(0, -10), (1, -8)], 3 * 3600, 8 * 3600));
    projection.day.firmware = FirmwareValidity::Valid;
    projection
}

/// Frames built by hand that stand for the states the dashboard tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Staged {
    /// A night mapping CCD 1.
    Mapping,
    /// A night qualifying a profile on the exact whole machine.
    Qualifying,
    /// A night reproducing a day event.
    Reproducing,
    /// The day, with a trial running.
    Day,
    /// An idle (R6) trial, which holds the frame.
    Idle,
    /// A trial a later boot outlived.
    Crashed,
    /// A crash detected on this boot.
    Recovering,
    /// Resting between two trials.
    Between,
    /// A dead end.
    Deadend,
    /// A stopped session.
    Stopped,
}

impl Staged {
    pub(crate) const ALL: [Self; 10] = [
        Self::Mapping,
        Self::Qualifying,
        Self::Reproducing,
        Self::Day,
        Self::Idle,
        Self::Crashed,
        Self::Recovering,
        Self::Between,
        Self::Deadend,
        Self::Stopped,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Mapping => "mapping",
            Self::Qualifying => "qualifying",
            Self::Reproducing => "reproducing",
            Self::Day => "day",
            Self::Idle => "idle",
            Self::Crashed => "crashed",
            Self::Recovering => "recovering",
            Self::Between => "between",
            Self::Deadend => "deadend",
            Self::Stopped => "stopped",
        }
    }

    pub(crate) fn hand(self) -> Hand {
        let mut cores = sixteen(|id| Some(if id < 8 { -10 } else { -8 }));
        let mut policy = two_ccds();
        let mut trial = None;
        let ids = |range: std::ops::Range<u8>| range.collect::<Vec<u8>>();
        match self {
            Self::Mapping | Self::Crashed | Self::Recovering => {
                load(&mut cores, 8..16);
                policy.night = Some(night(Some((Queue::Mapping, 2, Activity::Mapping))));
                if let Some(night) = policy.night.as_mut() {
                    night.mapping = vec![MappingProjection {
                        ccd: ccd(1),
                        cursor: Some(off(-20)),
                        bracket: None,
                    }];
                }
                trial = Some(mapping_trial(2, 1, -20, &ids(8..16)));
            }
            Self::Qualifying => {
                load(&mut cores, 0..16);
                let mut night = night(Some((Queue::Qualification, 4, Activity::Qualification)));
                night.qualification = Some(QualificationProgress {
                    profile: profile_of(&[(0, -10), (1, -8)]),
                    outstanding: vec![class(Regime::R1, &[0]), class(Regime::R2, &[8])],
                    witnessed: 3,
                    eligible_s: 900,
                });
                policy.night = Some(night);
                policy.qualifications = vec![QualificationProjection {
                    profile: profile_of(&[(0, -10), (1, -8)]),
                    purpose: QualificationPurpose::NextStep,
                    state: QualificationStage::Progress,
                    current: true,
                    eligible_s: 900,
                }];
                trial = Some(TrialView {
                    task: Some(TaskId {
                        queue: Queue::Qualification,
                        ordinal: 4,
                    }),
                    tested: Some(Tested::Exact {}),
                    activity: Some(Activity::Qualification),
                    ..running_trial(Regime::R2, &ids(0..16))
                });
            }
            Self::Reproducing => {
                load(&mut cores, 0..8);
                let mut night = night(Some((Queue::Reproduction, 5, Activity::Reproduction)));
                let source = DayEventId::Boot {
                    boot: BootId::from("7a1b2c3d-0000-0000-0000-000000000000".to_owned()),
                };
                night.reproductions = vec![ReproductionProjection {
                    source: source.clone(),
                    state: ReproductionState::Pending,
                    consumed_s: 1500,
                    remaining: vec![class(Regime::R1, &[0])],
                }];
                policy.night = Some(night);
                policy.events = vec![EventProjection {
                    event: source,
                    classification: Classification::Unexplained {},
                    resolution: Resolution::Unresolved {},
                    reproduction: ReproductionState::Pending,
                }];
                trial = Some(TrialView {
                    task: Some(TaskId {
                        queue: Queue::Reproduction,
                        ordinal: 5,
                    }),
                    ..running_trial(Regime::R1, &ids(0..8))
                });
            }
            Self::Day => {
                // An observing day: the night is closed, no core is loaded and no trial runs.
                let mut night = night(None);
                night.started = FIXED_NOW - SignedDuration::from_hours(8);
                night.closed_by = Some(seq(2));
                policy.night = Some(night);
                policy.day.last_step = Some((seq(5), step(StepKind::Promote { changed: ccd(0) })));
            }
            Self::Idle => {
                policy.night = Some(night(None));
                trial = Some(running_trial(Regime::R6, &[]));
            }
            Self::Between | Self::Deadend | Self::Stopped => {
                policy.night = Some(night(None));
            }
        }
        let mut hand = Hand::new(cores, policy);
        hand.trial = trial;
        match self {
            Self::Crashed => {
                if let Some(trial) = hand.trial.as_mut() {
                    trial.crashed = true;
                }
            }
            Self::Recovering => {
                hand.recover = Some(RecoveryView {
                    crash_at: FIXED_NOW,
                    reset: Some(ResetKind::Watchdog),
                    trial: hand.trial.take(),
                    end: None,
                });
            }
            Self::Between => hand.last = Some(ended_trial(Regime::R1)),
            Self::Deadend => {
                hand.last = Some(ended_trial(Regime::R1));
                hand.dead_end = Some(DeadEndView {
                    at: FIXED_NOW,
                    condition: DeadEndCondition::NoEvidence,
                    detail: "kernel log unreadable: permission denied".to_owned(),
                });
            }
            Self::Stopped => {
                hand.last = Some(ended_trial(Regime::R1));
                hand.stopped = Some(StopView {
                    at: FIXED_NOW,
                    reason: ShutdownReason::Signal,
                    saved: true,
                });
            }
            Self::Day => {
                hand.last = Some(ended_trial(Regime::R1));
                if let Some((_, step)) = &hand.policy.day.last_step {
                    hand.applied_steps.insert(step.decision);
                }
            }
            Self::Mapping | Self::Qualifying | Self::Reproducing | Self::Idle => {}
        }
        hand.history = vec![Entry::new(
            FIXED_NOW,
            Tag::Start,
            What::Said("session started on 16 cores".to_owned()),
            Tone::Plain,
        )];
        hand.log = vec![LogEntry {
            seq: seq(1),
            at: FIXED_NOW,
            tag: "session start".into(),
            msg: "session started on 16 cores".to_owned(),
        }];
        if self == Self::Day {
            hand.log.push(LogEntry {
                seq: seq(2),
                at: FIXED_NOW,
                tag: "day boot".into(),
                msg: "a boot closed the night".to_owned(),
            });
        }
        if self == Self::Stopped {
            hand.last_failure = Some(FIXED_NOW);
        }
        hand
    }
}
