use std::collections::{BTreeMap, BTreeSet};

use togi_journal::{Build, CycleEvent, Event, Payload, Seq, ShutdownReason, Skipped};
use togi_machine::{BootId, CoreId};

use crate::marks::Marks;
use crate::violation::{Check, Completion, Findings, Violation};
use crate::writes::Writes;

pub(crate) type Seen<'a> = BTreeMap<Seq, &'a Event>;

#[derive(Debug, Default)]
struct Conclusion {
    stopped: bool,
    dead_end_pending: bool,
    passed_full_cycles: u32,
}

/// `skipped` names the events a lenient read passed over: a gap they fill is no sequence violation, and a cause
/// naming one of them is an existing earlier sequence.
pub(crate) fn audit(events: &[Event], skipped: &Skipped, completion: Completion) -> Vec<Violation> {
    let mut auditor = Auditor {
        skipped: skipped.events().iter().map(|event| event.seq).collect(),
        ..Auditor::default()
    };
    for event in events {
        auditor.fold(event);
    }
    if completion == Completion::Finished {
        auditor.writes.finish(&mut auditor.findings);
        auditor.flush_rebuild();
        if let Some(last) = events.last() {
            if !auditor.conclusion.stopped {
                auditor.findings.add(
                    last.seq,
                    Check::Termination,
                    "finished run has neither a conclusion nor a dead end",
                );
            }
        }
    }
    auditor.findings.into_vec()
}

#[derive(Default)]
struct Auditor<'a> {
    findings: Findings,
    boot: Option<&'a BootId>,
    cores: Vec<CoreId>,
    previous: Option<Seq>,
    seen: Seen<'a>,
    skipped: BTreeSet<Seq>,
    marks: Marks,
    writes: Writes,
    conclusion: Conclusion,
    build: Option<&'a Build>,
    rebuilt: Option<Seq>,
}

impl<'a> Auditor<'a> {
    fn fold(&mut self, event: &'a Event) {
        if let Payload::SessionStart(start) = &event.payload {
            self.build = Some(&start.build);
            self.cores = start.cores.iter().map(|info| info.core).collect();
            self.cores.sort_unstable();
        }
        if self.boot != Some(&event.boot) {
            self.flush_rebuild();
            self.boot = Some(&event.boot);
            self.writes.new_boot(&self.cores);
        }
        self.check_sequence(event);
        self.check_causes(event);
        self.marks.fold(event, &self.seen);
        self.writes.fold(
            event,
            &self.seen,
            &self.cores,
            &self.marks,
            &mut self.findings,
        );
        self.fold_build(event);
        self.fold_conclusion(event);
        self.seen.insert(event.seq, event);
        self.previous = Some(event.seq);
    }

    fn check_sequence(&mut self, event: &Event) {
        let mut expected = self.previous.map_or(Some(Seq::FIRST), Seq::next);
        while let Some(seq) = expected.filter(|seq| self.skipped.contains(seq)) {
            expected = seq.next();
        }
        if expected != Some(event.seq) {
            self.findings.add(
                event.seq,
                Check::Sequence,
                format!(
                    "seq {} follows {}, not the next count",
                    event.seq,
                    self.previous
                        .map_or_else(|| "nothing".to_owned(), |seq| seq.to_string())
                ),
            );
        }
        if self.previous.is_none() && !matches!(event.payload, Payload::SessionStart(_)) {
            self.findings.add(
                event.seq,
                Check::Sequence,
                format!("the first event is {}, not session.start", event.kind()),
            );
        }
    }

    fn check_causes(&mut self, event: &Event) {
        for cause in &event.cause {
            let known = self.seen.contains_key(cause) || self.skipped.contains(cause);
            if !known || *cause >= event.seq {
                self.findings.add(
                    event.seq,
                    Check::Cause,
                    format!(
                        "{} cause {cause} is not an existing earlier sequence",
                        event.kind()
                    ),
                );
            }
        }
    }

    fn fold_conclusion(&mut self, event: &Event) {
        let conclusion = &mut self.conclusion;
        let previous = conclusion.stopped;
        conclusion.stopped = false;
        match &event.payload {
            Payload::DeadEnd(dead_end) => {
                if dead_end.detail.trim().is_empty() {
                    self.findings
                        .add(event.seq, Check::Termination, "dead end has no reason");
                }
                conclusion.dead_end_pending = true;
                conclusion.stopped = true;
            }
            Payload::Shutdown(shutdown) => {
                match shutdown.reason {
                    ShutdownReason::DeadEnd if !conclusion.dead_end_pending => {
                        self.findings.add(
                            event.seq,
                            Check::Termination,
                            "shutdown for a dead end follows no dead end",
                        );
                    }
                    ShutdownReason::Cycles
                    | ShutdownReason::FirstResult
                    | ShutdownReason::Command
                        if conclusion.dead_end_pending =>
                    {
                        self.findings.add(
                            event.seq,
                            Check::Termination,
                            format!(
                                "shutdown for {} while a recorded dead end awaits its own shutdown",
                                shutdown.reason
                            ),
                        );
                    }
                    ShutdownReason::Signal
                    | ShutdownReason::DeadEnd
                    | ShutdownReason::Cycles
                    | ShutdownReason::Command
                    | ShutdownReason::FirstResult => {}
                }
                if shutdown.reason == ShutdownReason::Cycles {
                    match shutdown.cycles {
                        None => self.findings.add(
                            event.seq,
                            Check::Termination,
                            "shutdown for completed cycles does not record how many",
                        ),
                        Some(requested) if conclusion.passed_full_cycles < requested.get() => {
                            self.findings.add(
                                event.seq,
                                Check::Termination,
                                format!(
                                    "shutdown after {requested} requested cycles follows only {} passed full cycles",
                                    conclusion.passed_full_cycles
                                ),
                            );
                        }
                        Some(_) => {}
                    }
                }
                if shutdown.reason != ShutdownReason::Signal {
                    conclusion.dead_end_pending = false;
                }
                conclusion.stopped = true;
            }
            Payload::SessionArchived(_) => conclusion.stopped = true,
            Payload::CommandReset(reset) => {
                conclusion.stopped = matches!(reset.target, togi_journal::ResetTarget::All);
            }
            Payload::SessionWarning(_)
            | Payload::BootLeaveReason(_)
            | Payload::BootSavedEntry(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayUncleanEnd(_) => {
                conclusion.stopped = previous;
            }
            Payload::CheckingCycle(cycle)
                if cycle.event == CycleEvent::End && cycle.passed && cycle.full =>
            {
                conclusion.passed_full_cycles += 1;
            }
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
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
            | Payload::TunerDecision(_)
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
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_) => {}
        }
    }

    fn fold_build(&mut self, event: &'a Event) {
        match &event.payload {
            Payload::ConfigLoaded(loaded) => {
                if self.rebuilt.is_some() && self.build == Some(&loaded.build) {
                    self.flush_rebuild();
                }
                self.rebuilt = None;
                self.build = Some(&loaded.build);
            }
            Payload::StateRebuilt(rebuilt) => {
                if !rebuilt.fields.iter().any(|field| field == "last_seq") {
                    self.flush_rebuild();
                    self.rebuilt = Some(event.seq);
                }
            }
            Payload::Shutdown(_) | Payload::DeadEnd(_) | Payload::SessionArchived(_) => {
                self.flush_rebuild();
            }
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
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
            | Payload::TunerDecision(_)
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
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayUncleanEnd(_) => {}
        }
    }

    fn flush_rebuild(&mut self) {
        if let Some(seq) = self.rebuilt.take() {
            self.findings.add(
                seq,
                Check::Replay,
                "state.rebuilt records disagreement with a current state.json snapshot",
            );
        }
    }
}
