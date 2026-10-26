//! What the journal's defects ask of the run: the findings it records and the stop a too-aggressive one makes.

use togi_journal::{DeadEnd, DeadEndCondition, DefectDirection, DefectFound, Payload, Seq};
use togi_machine::CoreSet;

use crate::error::SessionError;
use crate::session::{Session, Stop};

impl Session<'_> {
    /// Records the defects the journal's decisions match. A too-aggressive finding is unresolved for the rest of the
    /// session, since only `togi reset --all` starts a fresh one, so it is a dead end: no nonzero work runs while it
    /// stands. A too-cautious finding lets the night continue.
    pub(crate) fn check_defects(&mut self) -> Result<Option<Stop>, SessionError> {
        let found: Vec<_> = self
            .appender
            .defects()
            .new_findings()
            .map(|finding| (finding.entry, finding.cores, finding.decisions.to_vec()))
            .collect();
        for (entry, cores, decisions) in found {
            let payload = DefectFound {
                id: entry.id,
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
        let Some((seq, dead_end)) = self
            .appender
            .defects()
            .findings()
            .find(|finding| finding.direction == DefectDirection::TooAggressive)
            .map(|finding| {
                let detail = format!(
                    "defect {} ({}) affected decisions [{}] on cores {}; togi reset --all archives this session and starts fresh",
                    finding.id,
                    finding.title,
                    seq_list(finding.decisions),
                    core_list(finding.cores),
                );
                (
                    finding.seq,
                    DeadEnd {
                        condition: DeadEndCondition::Defect,
                        core: None,
                        detail,
                        action: None,
                    },
                )
            })
        else {
            return Ok(None);
        };
        self.dead_end(dead_end, &[seq]).map(Some)
    }
}

fn seq_list(decisions: &[Seq]) -> String {
    let named: Vec<String> = decisions.iter().map(|seq| format!("#{seq}")).collect();
    named.join(" ")
}

fn core_list(cores: CoreSet) -> String {
    let named: Vec<String> = cores.iter().map(|core| core.to_string()).collect();
    format!("[{}]", named.join(" "))
}

#[cfg(test)]
mod tests;
