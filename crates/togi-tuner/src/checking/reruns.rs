use std::collections::VecDeque;
use std::num::NonZeroU16;

use togi_journal::{Activity, Seq, TrialIntent};
use togi_machine::Condition;

use crate::failures::load_of;
use crate::state::Run;
use crate::{
    Action, ClassKey, Requirement, RequirementKind, TrialContext, TrialPlan, Window, WorkloadChoice,
};

/// A failed class the checking profile has to pass again before checking goes on: the class of
/// the failure an offset-changing decision answered, and that failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Obligation {
    pub(crate) class: ClassKey,
    pub(crate) failure: Seq,
}

/// The reruns owed, oldest first, and the carried facts that answered reruns since the last
/// cycle or round decision cited them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Reruns {
    fifo: VecDeque<Obligation>,
    citations: Vec<Seq>,
}

impl Reruns {
    pub(crate) fn head(&self) -> Option<Obligation> {
        self.fifo.front().copied()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.fifo.is_empty()
    }

    pub(crate) fn owed(&self) -> impl Iterator<Item = Obligation> + '_ {
        self.fifo.iter().copied()
    }

    pub(crate) fn citations(&self) -> &[Seq] {
        &self.citations
    }

    pub(crate) fn consume_citations(&mut self) {
        self.citations.clear();
    }

    fn push(&mut self, obligation: Obligation) {
        self.fifo.push_back(obligation);
    }

    fn retire_head(&mut self, carried: impl IntoIterator<Item = Seq>) {
        self.fifo.pop_front();
        for seq in carried {
            if !self.citations.contains(&seq) {
                self.citations.push(seq);
            }
        }
    }

    /// The newest failure queued for the class of `head`.
    fn newest(&self, head: Obligation) -> Seq {
        self.fifo
            .iter()
            .filter(|obligation| obligation.class == head.class)
            .map(|obligation| obligation.failure)
            .max()
            .unwrap_or(head.failure)
    }
}

/// What the head obligation still needs: passes of a class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Need {
    pub(crate) class: ClassKey,
    pub(crate) needed: NonZeroU16,
}

enum Answer {
    Owed(Need),
    Answered(Vec<Seq>),
}

impl Run {
    /// An offset-changing decision answered the first failure among `cause` that is still
    /// pending: its class is owed a rerun.
    pub(crate) fn queue_rerun(&mut self, cause: &[Seq]) {
        let found = cause.iter().find_map(|&seq| {
            self.failures.pending(seq).map(|pending| Obligation {
                class: pending.class,
                failure: seq,
            })
        });
        if let Some(obligation) = found {
            self.reruns.push(obligation);
        }
    }

    fn rerun_answer(&self, obligation: Obligation) -> Answer {
        let short = obligation.class.with_duration(self.config.short_trial);
        let needed = self.config.needed;
        let owed = |class, needed| Answer::Owed(Need { class, needed });
        let Some(profile) = self.checking.profile() else {
            return owed(short, needed);
        };
        let window = Window::LiveOrCarried {
            since: Some(obligation.failure),
        };
        let passed = self
            .evidence
            .count(short, &profile, window, needed)
            .unwrap_or_default();
        if passed.valid < u32::from(needed.get()) {
            return owed(short, needed);
        }
        let mut witnesses = passed.witnesses;
        if obligation.class.duration() != self.config.short_trial {
            let long = self
                .evidence
                .count(obligation.class, &profile, window, NonZeroU16::MIN)
                .unwrap_or_default();
            let Some(&first) = long.witnesses.first() else {
                return owed(obligation.class, NonZeroU16::MIN);
            };
            witnesses.push(first);
        }
        Answer::Answered(witnesses)
    }

    /// Retires every head obligation the evidence now answers, keeping the carried facts that
    /// answered them for the next cycle or round decision to cite.
    pub(crate) fn retire_reruns(&mut self) {
        while let Some(head) = self.reruns.head() {
            let Answer::Answered(witnesses) = self.rerun_answer(head) else {
                break;
            };
            let carried: Vec<Seq> = witnesses
                .into_iter()
                .filter(|&seq| self.evidence.carried_source(seq).is_some())
                .collect();
            self.reruns.retire_head(carried);
        }
    }

    /// The class and pass count the head obligation needs next.
    pub(crate) fn rerun_need(&self, head: Obligation) -> Option<Need> {
        match self.rerun_answer(head) {
            Answer::Owed(need) => Some(need),
            Answer::Answered(_) => None,
        }
    }

    pub(crate) fn rerun_requirement(head: Obligation, need: Need) -> Requirement {
        Requirement {
            kind: RequirementKind::Rerun,
            class: need.class,
            window: Window::LiveOrCarried {
                since: Some(head.failure),
            },
            needed: need.needed,
            step: None,
            part: None,
        }
    }

    /// The trial that answers the head obligation: its class run together on the checking
    /// profile, citing the newest failure queued for that class.
    pub(crate) fn rerun_action(&self) -> Option<Action> {
        let head = self.reruns.head()?;
        let profile = self.checking.profile()?;
        let need = self.rerun_need(head)?;
        let requirement = Self::rerun_requirement(head, need);
        let cited = self.reruns.newest(head);
        if let Some(plan) = self.serve_checking_retry(&requirement, vec![cited]) {
            return Some(Action::RunTrial(plan));
        }
        let class = need.class;
        Some(Action::RunTrial(TrialPlan {
            regime: class.regime(),
            workload: WorkloadChoice::Frozen(class.workload()),
            load: load_of(class),
            condition: Condition::Together,
            activity: Activity::Checking,
            profile,
            duration: class.duration(),
            context: TrialContext::Rerun {
                failure: head.failure,
            },
            requirement,
            retry: false,
            cause: vec![cited],
        }))
    }

    /// The requirement of a rerun intent, when it is the head obligation's next trial.
    pub(crate) fn rerun_context(
        &self,
        intent: &TrialIntent,
        class: ClassKey,
    ) -> Option<Requirement> {
        let head = self.reruns.head()?;
        let need = self.rerun_need(head)?;
        let matches = intent.rerun
            && intent.cycle.is_none()
            && intent.step.is_none()
            && class == need.class
            && Self::together_shape(intent, class);
        matches.then(|| Self::rerun_requirement(head, need))
    }

    /// The requirement a saved rerun retry would carry now: its obligation still the head, and
    /// the class still the one the head needs.
    pub(crate) fn rerun_retry_requirement(
        &self,
        plan: &TrialPlan,
        failure: Seq,
    ) -> Option<Requirement> {
        let head = self.reruns.head().filter(|head| head.failure == failure)?;
        let need = self.rerun_need(head)?;
        let WorkloadChoice::Frozen(workload) = plan.workload else {
            return None;
        };
        let class = ClassKey::new(workload, plan.load.cores(), plan.duration);
        (class == need.class).then(|| Self::rerun_requirement(head, need))
    }
}
