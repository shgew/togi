use togi_journal::{
    CorePhase, CoreTransition, HuntEnd, HuntGroup, HuntOutcome, HuntStart, Outcome, Seq,
};
use togi_machine::{Condition, Regime};

use crate::testkit::{Journal, Start};
use crate::{Action, DecisionPayload, TrialPlan};

pub(super) fn hunt_session(offsets: &[i64]) -> Journal {
    let ids: Vec<u8> = (0..offsets.len())
        .map(|id| u8::try_from(id).unwrap())
        .collect();
    let starts: Vec<Start> = offsets.iter().map(|&o| Start::has_room(o)).collect();
    let mut journal = Journal::new(&ids, &starts);
    journal.load(&crate::testkit::config(
        r#"{"durations":{"checking_idle_s":120}}"#,
    ));
    journal.profile_change(offsets);
    journal.ranking(&ids);
    journal
}

impl Journal {
    pub(super) fn together_failure(
        &mut self,
        regime: Regime,
        workload: usize,
        profile: &[i64],
    ) -> Seq {
        let cores: Vec<u8> = self
            .run()
            .topo_ref()
            .order()
            .iter()
            .map(|c| c.get())
            .collect();
        let duration = self.run().config.checking_idle.get();
        let plan = self.manual(
            regime,
            workload,
            crate::Load::Many(crate::testkit::set(&cores)),
            Condition::Together,
            profile,
            duration,
        );
        self.trial(&plan, Outcome::Failure);
        let decision = self.next_decision();
        assert!(matches!(decision.payload, DecisionPayload::Failure(_)));
        self.decide(decision)
    }

    pub(super) fn hunt_start_payload(&self) -> (HuntStart, Vec<Seq>) {
        match self.next() {
            Action::Decide(decision) => match decision.payload {
                DecisionPayload::HuntStart(start) => (start, decision.cause),
                other => panic!("expected a hunt start, got {other:?}"),
            },
            other => panic!("expected a hunt start, got {other:?}"),
        }
    }

    pub(super) fn start_hunt(&mut self) -> (Seq, HuntStart) {
        let (start, cause) = self.hunt_start_payload();
        let seq = self.append(start.clone().into(), &cause);
        (seq, start)
    }

    pub(super) fn group_plan(&self) -> (HuntGroup, Vec<Seq>) {
        match self.next() {
            Action::Decide(decision) => match decision.payload {
                DecisionPayload::HuntGroup(group) => (group, decision.cause),
                other => panic!("expected a hunt group, got {other:?}"),
            },
            other => panic!("expected a hunt group, got {other:?}"),
        }
    }

    pub(super) fn run_hunt(
        &mut self,
        limit: usize,
        mut oracle: impl FnMut(&TrialPlan) -> Outcome,
    ) -> HuntEnd {
        for _ in 0..limit {
            let action = self.step(&mut oracle);
            if let Action::Decide(decision) = action {
                if let DecisionPayload::HuntEnd(end) = decision.payload {
                    return end;
                }
            }
        }
        panic!("the hunt did not end within {limit} actions");
    }
}

pub(super) fn ended(end: &HuntEnd, result: HuntOutcome) {
    assert_eq!(end.result, result, "{end:?}");
}

/// A passed full cycle at `checked` that leaves phase 1 open: core 00 searches while it runs, so
/// a later failure is hunted rather than blamed on a deepened core.
pub(super) fn passed_cycle(journal: &mut Journal, checked: &[i64], back_to: &[i64]) -> Seq {
    let first = crate::tests::core(0);
    let core = journal.run().core(first);
    let leave = |from: CorePhase, to: CorePhase| CoreTransition {
        core: first,
        from: Some(from),
        to,
        offset: core.offset,
        pass: None,
        failure_point: None,
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    };
    let phase = core.phase.expect("the core has a phase");
    journal.core_phase(leave(phase, CorePhase::Search));
    journal.profile_change(checked);
    journal.cycle_start(1, &[]);
    let end = journal.cycle_end(1, true, true);
    journal.core_phase(leave(CorePhase::Search, phase));
    journal.profile_change(back_to);
    end
}
