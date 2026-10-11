//! What the journal's defects ask of the run: the findings it records, the questions it puts to the person at the
//! terminal and the resets an answer queues.

use togi_defect::DefectId;
use togi_journal::{
    CommandReset, DeadEnd, DeadEndCondition, DefectAnswer, DefectAnswered, DefectDirection,
    DefectFound, Payload, ResetTarget, Seq,
};
use togi_machine::CoreSet;

use crate::error::SessionError;
use crate::session::{PromptError, Session, Stop};

impl Session {
    /// Records the defects the journal's decisions match, queues the resets of every answered "yes", and puts each
    /// unanswered finding to the prompt. Without one, a finding that says the tuner was too aggressive is a dead end.
    pub(crate) fn check_defects(&mut self) -> Result<Option<Stop>, SessionError> {
        let found: Vec<_> = self
            .appender
            .defects()
            .new_findings()
            .map(|finding| (finding.entry, finding.cores, finding.decisions.to_vec()))
            .collect();
        for (entry, cores, decisions) in found {
            let payload = DefectFound {
                id: entry.id.0,
                title: entry.title.to_owned(),
                detail: entry.detail.to_owned(),
                pr: entry.pr,
                direction: entry.direction,
                cores,
                decisions: decisions.clone(),
            };
            self.appender
                .append(Payload::DefectFound(payload), &decisions)?;
        }
        self.queue_resets()?;
        let unanswered: Vec<DefectId> = self
            .appender
            .defects()
            .unanswered()
            .filter(|finding| finding.entry.is_some())
            .map(|finding| finding.id)
            .collect();
        for id in unanswered {
            if let Some(stop) = self.settle_finding(id)? {
                return Ok(Some(stop));
            }
        }
        Ok(None)
    }

    fn settle_finding(&mut self, id: DefectId) -> Result<Option<Stop>, SessionError> {
        let Some(finding) = self
            .appender
            .defects()
            .unanswered()
            .find(|finding| finding.id == id)
        else {
            return Ok(None);
        };
        let (seq, cores, direction, title) = (
            finding.seq,
            finding.cores,
            finding.direction,
            finding.title.to_owned(),
        );
        let Some(prompt) = self.prompt.as_mut() else {
            if direction != DefectDirection::TooAggressive {
                return Ok(None);
            }
            let dead_end = DeadEnd {
                condition: DeadEndCondition::Defect,
                core: None,
                detail: format!(
                    "defect {} ({title}) affected cores {}; run in a terminal to decide whether to reset them",
                    id.get(),
                    core_list(cores)
                ),
                action: None,
            };
            return self.dead_end(dead_end, &[seq]).map(Some);
        };
        let reset = match prompt.reset_cores(&finding) {
            Ok(reset) => reset,
            Err(PromptError::Cancelled) => {
                return Ok(Some(self.signal_stop()));
            }
            Err(PromptError::Io(source)) => {
                return Err(SessionError::Prompt {
                    defect: id.get(),
                    source,
                });
            }
        };
        let answered = DefectAnswered {
            id: id.0,
            cores,
            answer: if reset {
                DefectAnswer::Yes
            } else {
                DefectAnswer::No
            },
        };
        self.appender
            .append(Payload::DefectAnswered(answered), &[seq])?;
        if reset {
            self.queue_resets()?;
        }
        Ok(None)
    }

    fn signal_stop(&mut self) -> Stop {
        self.record_shutdown(&Stop::Signal);
        Stop::Signal
    }

    /// A reset for every core a "yes" answer named that has none yet, each citing the answer.
    fn queue_resets(&mut self) -> Result<(), SessionError> {
        let missing: Vec<(Seq, _)> = self.appender.defects().missing_resets().collect();
        for (answer, core) in missing {
            let reset = CommandReset {
                target: ResetTarget::Core(core),
            };
            self.appender
                .append(Payload::CommandReset(reset), &[answer])?;
        }
        Ok(())
    }
}

fn core_list(cores: CoreSet) -> String {
    let named: Vec<String> = cores.iter().map(|core| core.to_string()).collect();
    format!("[{}]", named.join(" "))
}

#[cfg(test)]
mod tests;
