//! The night scheduler (`docs/spec/tuner.md`, Scheduling): a pure query over the wall
//! seconds each queue consumed in the open night and the work each queue holds. A night has no deadline and
//! no budget: the shares are soft shares of its elapsed observed time.

use crate::UnitProfile;
use togi_journal::Seq;

use super::edge::MappingTarget;
use super::matrix::{AttemptScope, Cell};

/// Whole wall-clock seconds.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct WallSecs(u64);

impl WallSecs {
    #[must_use]
    pub const fn new(secs: u64) -> Self {
        Self(secs)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// `percent`% of this span, rounded down.
    #[must_use]
    pub const fn percent(self, percent: u8) -> Self {
        Self(self.0.saturating_mul(percent as u64) / 100)
    }
}

/// The night's work queues in the order the scheduler visits them.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Queue {
    Reproduction,
    Qualification,
    Mapping,
    Requalification,
}

const VISIT_ORDER: [Queue; 4] = [
    Queue::Reproduction,
    Queue::Qualification,
    Queue::Mapping,
    Queue::Requalification,
];

/// Due mapping's soft share of the night's elapsed observed time E.
pub const MAPPING_SHARE_PCT: u8 = 10;
/// Due monthly re-qualification's soft share of E.
pub const REQUALIFICATION_SHARE_PCT: u8 = 10;
/// Reproduction's soft share of E.
pub const REPRODUCTION_SHARE_PCT: u8 = 25;
/// Next-step qualification's soft share of E.
pub const QUALIFICATION_SHARE_PCT: u8 = 75;

/// Each queue's soft share of E as a percentage, explicit so a simulator can vary them. A share is neither a
/// cap nor a guaranteed allocation: a queue at or over it yields only to a due queue under its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Budgets {
    pub mapping: u8,
    pub requalification: u8,
    pub reproduction: u8,
    pub qualification: u8,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            mapping: MAPPING_SHARE_PCT,
            requalification: REQUALIFICATION_SHARE_PCT,
            reproduction: REPRODUCTION_SHARE_PCT,
            qualification: QUALIFICATION_SHARE_PCT,
        }
    }
}

impl Budgets {
    /// A queue's share of E as a percentage, by kind.
    #[must_use]
    pub const fn share(self, queue: Queue) -> u8 {
        match queue {
            Queue::Reproduction => self.reproduction,
            Queue::Qualification => self.qualification,
            Queue::Mapping => self.mapping,
            Queue::Requalification => self.requalification,
        }
    }
}

/// Wall seconds each queue and the filler have consumed this night.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Consumed {
    pub reproduction: WallSecs,
    pub qualification: WallSecs,
    pub mapping: WallSecs,
    pub requalification: WallSecs,
    pub filler: WallSecs,
}

impl Consumed {
    #[must_use]
    pub const fn get(&self, queue: Queue) -> WallSecs {
        match queue {
            Queue::Reproduction => self.reproduction,
            Queue::Qualification => self.qualification,
            Queue::Mapping => self.mapping,
            Queue::Requalification => self.requalification,
        }
    }

    /// The night's elapsed observed time E: everything its tasks consumed, filler included.
    #[must_use]
    pub const fn total(&self) -> WallSecs {
        self.reproduction
            .saturating_add(self.qualification)
            .saturating_add(self.mapping)
            .saturating_add(self.requalification)
            .saturating_add(self.filler)
    }
}

/// Why the exact current D is re-qualified.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequalificationDue {
    /// Thirty days since its last complete qualification.
    Elapsed,
    /// Its identity was invalidated.
    Invalidated,
}

/// Why a task runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Reason {
    Reproduction { event: Seq, attempt: AttemptScope },
    Qualification,
    Mapping { target: MappingTarget },
    Requalification { due: RequalificationDue },
}

/// One queue's outstanding work: the cells still to launch, in order, at one whole CCD profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Work {
    pub profile: UnitProfile,
    pub cells: Vec<Cell>,
    pub reason: Reason,
}

/// Where an event's reproduction night stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReproductionStage {
    /// The attempt has outstanding cells and this night is its night.
    Open(Work),
    /// Every profile's attempt ended, clean or at its first failure.
    Concluded,
    /// An earlier night, including one a crash interrupted, left cells outstanding: the event's
    /// one night is over and it is unreproduced.
    Lapsed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reproduction {
    /// The oldest CPU-suspect event with an open reproduction task.
    pub event: Seq,
    pub stage: ReproductionStage,
}

/// Everything the scheduler reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Inputs {
    pub budgets: Budgets,
    /// What each queue consumed in the open night.
    pub consumed: Consumed,
    pub reproduction: Option<Reproduction>,
    pub qualification: Option<Work>,
    pub mapping: Option<Work>,
    pub requalification: Option<Work>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NightTask {
    Run {
        queue: Queue,
        cell: Cell,
        profile: UnitProfile,
        reason: Reason,
    },
    /// Close the event's reproduction as unreproduced; no trial runs.
    Unreproduced { event: Seq },
    /// No queue has due work: the filler runs.
    Idle,
}

/// Work that can run: cells outstanding at a profile that is not all zero. An all-zero profile is never
/// run, whatever queue holds it.
fn runnable(work: Option<&Work>) -> Option<&Work> {
    work.filter(|work| !work.cells.is_empty() && !work.profile.is_zero())
}

/// The queues holding work this visit, in visiting order.
fn due(inputs: &Inputs) -> Vec<(Queue, &Work)> {
    let reproduction =
        inputs
            .reproduction
            .as_ref()
            .and_then(|reproduction| match &reproduction.stage {
                ReproductionStage::Open(work) => Some(work),
                ReproductionStage::Concluded | ReproductionStage::Lapsed => None,
            });
    VISIT_ORDER
        .into_iter()
        .filter_map(|queue| {
            let work = match queue {
                Queue::Reproduction => reproduction,
                Queue::Qualification => inputs.qualification.as_ref(),
                Queue::Mapping => inputs.mapping.as_ref(),
                Queue::Requalification => inputs.requalification.as_ref(),
            };
            runnable(work).map(|work| (queue, work))
        })
        .collect()
}

/// Whether `queue` consumed less than its share of E. At E = 0 no queue is under its share.
fn under_share(inputs: &Inputs, queue: Queue) -> bool {
    let elapsed = u128::from(inputs.consumed.total().get());
    u128::from(inputs.consumed.get(queue).get()) * 100
        < u128::from(inputs.budgets.share(queue)) * elapsed
}

/// The next task of the open night. A queue at or over its share of the night's elapsed observed time
/// yields only while another queue with due work is under its own share; otherwise the first due queue in
/// priority order runs its first outstanding cell. No cell is checked against remaining time: a night has no
/// deadline, and an operator end mid-cell interrupts the trial. With nothing due the filler runs.
#[must_use]
pub fn next_task(inputs: &Inputs) -> NightTask {
    if let Some(Reproduction {
        event,
        stage: ReproductionStage::Lapsed,
    }) = &inputs.reproduction
    {
        return NightTask::Unreproduced { event: *event };
    }
    let pending = due(inputs);
    let chosen = pending
        .iter()
        .find(|(queue, _)| under_share(inputs, *queue))
        .or_else(|| pending.first());
    let Some(&(queue, work)) = chosen else {
        return NightTask::Idle;
    };
    work.cells
        .first()
        .map_or(NightTask::Idle, |cell| NightTask::Run {
            queue,
            cell: *cell,
            profile: work.profile.clone(),
            reason: work.reason,
        })
}
