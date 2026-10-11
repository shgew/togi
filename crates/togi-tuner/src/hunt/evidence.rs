use togi_journal::{HuntStage, Inference, Seq};

use super::plan::GroupOutcome;
use super::state::{Active, Answer, GroupRecord};
use crate::state::Run;
use crate::{ClassKey, Profile, Window};

impl Run {
    pub(crate) fn is_carried(&self, seq: Seq) -> bool {
        self.evidence.carried_source(seq).is_some()
    }

    pub(crate) fn hunt_since(&self, stage: HuntStage, own: Seq) -> Option<Seq> {
        match stage {
            HuntStage::Part | HuntStage::Complement => self.hunts.reset,
            HuntStage::Full | HuntStage::Locate | HuntStage::Probe => Some(own),
        }
    }

    pub(crate) fn evidence_answer(
        &self,
        hunt: &Active,
        class: ClassKey,
        profile: &Profile,
        since: Option<Seq>,
    ) -> Answer {
        let needed = hunt.needed;
        let failure = self
            .evidence
            .failing_seq(class, profile, since, needed)
            .ok()
            .flatten();
        if let Some(failure) = failure {
            return Answer {
                outcome: Some(GroupOutcome::Failed),
                failure: Some(failure),
                carried: Vec::new(),
                passes: 0,
            };
        }
        let Ok(count) =
            self.evidence
                .count(class, profile, Window::LiveOrCarried { since }, needed)
        else {
            return Answer::default();
        };
        if count.valid < u32::from(needed.get()) {
            return Answer::default();
        }
        Answer {
            outcome: Some(GroupOutcome::Passed),
            failure: None,
            passes: count.valid,
            carried: count
                .witnesses
                .into_iter()
                .filter(|&seq| self.is_carried(seq))
                .collect(),
        }
    }

    pub(crate) fn answer_of(&self, hunt: &Active, group: &GroupRecord) -> Answer {
        if group.payload.skipped {
            return Answer {
                outcome: Some(GroupOutcome::Skipped),
                ..Answer::default()
            };
        }
        if let Some(inferred) = group.payload.inferred {
            let own = group.own.clone().unwrap_or_default();
            return Answer {
                outcome: Some(match inferred {
                    Inference::Pass => GroupOutcome::Passed,
                    Inference::Failure => GroupOutcome::Failed,
                }),
                ..own
            };
        }
        let since = self.hunt_since(group.payload.stage, group.seq);
        self.evidence_answer(hunt, group.class, &group.profile, since)
    }

    pub(crate) fn inferred_evidence(&self, hunt: &Active, group: &GroupRecord) -> Answer {
        let since = self.hunt_since(
            group.payload.stage,
            match group.payload.stage {
                HuntStage::Part | HuntStage::Complement => group.seq,
                HuntStage::Full | HuntStage::Locate | HuntStage::Probe => hunt.seq,
            },
        );
        match group.payload.inferred {
            Some(Inference::Failure) => Answer {
                outcome: Some(GroupOutcome::Failed),
                failure: self
                    .evidence
                    .newest_failure(group.class, &group.profile, None)
                    .ok()
                    .flatten(),
                carried: Vec::new(),
                passes: 0,
            },
            Some(Inference::Pass) | None => {
                self.evidence_answer(hunt, group.class, &group.profile, since)
            }
        }
    }

    pub(crate) fn cite_new(cause: &mut Vec<Seq>, seq: Seq) {
        if !cause.contains(&seq) {
            cause.push(seq);
        }
    }

    pub(crate) fn cite_carried(&self, cause: &mut Vec<Seq>, seqs: impl IntoIterator<Item = Seq>) {
        for seq in seqs {
            if self.is_carried(seq) {
                Self::cite_new(cause, seq);
            }
        }
    }

    pub(crate) fn cite_group(&self, hunt: &Active, index: usize, cause: &mut Vec<Seq>) {
        let Some(group) = hunt.groups.get(index) else {
            return;
        };
        self.cite_carried(cause, group.inherited.iter().copied());
        if group.payload.inferred == Some(Inference::Failure) {
            Self::cite_new(cause, group.seq);
            return;
        }
        if group.payload.skipped || group.payload.inferred.is_some() {
            return;
        }
        if hunt.settled(index) {
            if let Some(answer) = &group.closing {
                match answer.failure {
                    Some(failure) => Self::cite_new(cause, failure),
                    None => self.cite_carried(cause, answer.carried.iter().copied()),
                }
            }
            return;
        }
        let answer = self.answer_of(hunt, group);
        match answer.failure {
            Some(failure) => {
                let before = group
                    .closing
                    .as_ref()
                    .filter(|_| index + 1 < hunt.groups.len())
                    .and_then(|closing| closing.failure);
                Self::cite_new(cause, before.unwrap_or(failure));
            }
            None => self.cite_carried(cause, answer.carried.iter().copied()),
        }
    }

    pub(crate) fn passes_of(&self, hunt: &Active, group: &GroupRecord) -> u32 {
        let since = self.hunt_since(
            group.payload.stage,
            if group.payload.inferred.is_some() {
                hunt.seq
            } else {
                group.seq
            },
        );
        self.evidence
            .count(
                group.class,
                &group.profile,
                Window::LiveOrCarried { since },
                hunt.needed,
            )
            .map_or(0, |count| count.valid)
    }
}
