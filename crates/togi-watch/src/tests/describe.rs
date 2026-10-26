//! The history as the facts describe it: one event, or the events before it, turned into a line.

use togi_journal::{
    Binding, DeadEnd, DeadEndCondition, Edge, Mce, QualificationState, Shutdown, ShutdownReason,
    StepKind,
};
use togi_machine::{BankType, CcdId, CpuId, Signal};

use super::kit::*;
use super::words::{
    cap_event, edge_event, hold_event, plan_event, qualification_event, step_event,
};
use crate::words::Tag;

fn last(h: &Hist) -> String {
    h.sentences().pop().unwrap()
}

#[test]
fn a_resumed_session_is_not_a_resumed_session_when_it_crashed() {
    let mut h = Hist::three();
    h.push(config_loaded());
    assert_eq!(h.sentences().len(), 1);
    h.push(config_loaded());
    assert_eq!(last(&h), "resume: session resumed");
    h.boot = "next".to_owned();
    h.push(crash("boot", None).into());
    assert_eq!(
        h.sentences()[1..],
        ["crash: idle with the offsets applied, rebooted"]
    );
}

#[test]
fn the_kinds_with_a_line_of_their_own() {
    let mut h = Hist::three();
    h.push(
        Mce {
            cpu: CpuId::from(3),
            core: None,
            bank: 1,
            bank_type: BankType::ExecutionUnit,
            corrected: true,
            monotonic_ns: None,
            from_boot: None,
            trial: None,
            between_trials: true,
            lines: Vec::new(),
        }
        .into(),
    );
    assert_eq!(
        last(&h),
        "mce: logical CPU 3 corrected error between trials (recorded only)"
    );
    h.push(
        DeadEnd {
            condition: DeadEndCondition::Smu,
            core: None,
            detail: "the SMU refused".to_owned(),
            action: None,
        }
        .into(),
    );
    assert_eq!(last(&h), "stop: dead end: the SMU refused");
    h.push(
        Shutdown {
            reason: ShutdownReason::Signal,
            kernel_cursor: None,
            kernel_error: None,
            cycles: None,
        }
        .into(),
    );
    assert_eq!(last(&h), "stop: stopped");
}

#[test]
fn a_failure_says_which_core_failed_where_or_that_the_machine_idled() {
    let mut h = Hist::three();
    let mut named = failure(Signal::ComputationError, Some("t"));
    named.attribution = togi_journal::Attribution::Attributed;
    named.core = Some(core(2));
    named.offset = Some(off(-18));
    h.push(named.into());
    assert_eq!(last(&h), "fail: core 02 fails at -18");
    h.push(failure(Signal::Stall, None).into());
    assert_eq!(
        last(&h),
        "fail: idle with the offsets applied, the backend stalled"
    );
}

#[test]
fn a_failure_that_names_a_known_failure_is_worded_like_any_other() {
    let mut h = Hist::three();
    let earlier = h.push(warning("an earlier event"));
    let mut again = failure(Signal::ComputationError, Some("t"));
    again.known_failure = Some(earlier);
    h.push(again.into());
    assert_eq!(
        h.sentences().len(),
        2,
        "the old skip line is gone: {:?}",
        h.sentences()
    );
}

#[test]
fn a_night_reads_in_order_from_plan_to_promotion() {
    let mut h = Hist::three();
    h.push(plan_event());
    h.push(edge_event(
        Edge::Unknown {},
        Edge::Observed {
            offset: off(-23),
            witness: seq(3),
        },
    ));
    h.push(cap_event(
        Binding::EdgeMargin {},
        togi_journal::Slope::Fallback {
            beta: togi_journal::Finite::new(0.2).unwrap(),
        },
    ));
    h.push(qualification_event(QualificationState::Start {
        outstanding: Vec::new(),
    }));
    h.push(qualification_event(QualificationState::Complete {
        witnesses: Vec::new(),
    }));
    h.push(step_event(StepKind::Promote {
        changed: CcdId::new(0).unwrap(),
    }));
    let sentences = h.sentences();
    let tags: Vec<&str> = sentences
        .iter()
        .map(|sentence| sentence.split(':').next().unwrap())
        .collect();
    assert_eq!(
        tags,
        [
            "start", "night", "edge", "cap", "qualify", "qualify", "step"
        ]
    );
    assert_eq!(
        sentences[3],
        "cap: CCD0 cap 0 → -15 · edge margin · fallback slope"
    );
    assert_eq!(
        sentences[4],
        "qualify: qualifying CCD0 -10, CCD1 -5 (next step) · 0 cells to witness"
    );
    assert_eq!(sentences[6], "step: promote CCD0 to CCD0 -15, CCD1 -5");
}

#[test]
fn a_hold_that_waits_adds_no_line_and_a_reset_hold_does() {
    let mut h = Hist::three();
    h.push(step_event(StepKind::Hold {
        blocked_by: vec![togi_journal::HoldReason::Frozen {}],
    }));
    h.push(hold_event(togi_journal::HoldChange::Credited {}));
    assert_eq!(h.sentences().len(), 1);
    h.push(hold_event(togi_journal::HoldChange::Reset {
        cause: seq(2),
    }));
    assert_eq!(last(&h), "hold: hold at CCD0 -10 reset · 2h00m eligible");
    assert_eq!(h.facts.history.entries.last().unwrap().tag, Tag::Hold);
}
