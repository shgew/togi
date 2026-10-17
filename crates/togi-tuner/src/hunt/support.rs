use std::num::NonZeroU32;

use togi_journal::{
    Attribution, CorePhase, CoreTransition, HuntEnd, HuntGroup, HuntOutcome, HuntStart, Member,
    Outcome, Seq,
};
use togi_machine::{Condition, Regime, workloads};

use crate::testkit::{Journal, Start, config, set};
use crate::tests::{core, off};
use crate::{Action, DecisionPayload, TrialPlan};

pub(crate) const SHORT: u32 = 120;

pub(crate) fn ids(count: usize) -> Vec<u8> {
    (0..count).map(|id| u8::try_from(id).unwrap()).collect()
}

/// `count` cores at their -30 limit, each known to fail at -31, with the ranking read.
pub(crate) fn limit_session(count: usize) -> Journal {
    let ids = ids(count);
    let starts = vec![Start::at_limit(-30).fail(-31); count];
    let mut j = Journal::new(&ids, &starts);
    j.profile_change(&vec![-30; count]);
    j.ranking(&ids);
    j
}

/// A `limit_session` whose all-core R6 trial of `duration` seconds failed together, with the
/// hunt for it started.
pub(crate) fn hunt_of(count: usize, duration: u32) -> Journal {
    let mut j = limit_session(count);
    if j.run().config.checking_idle.get() != duration {
        j.load(&config(&format!(
            r#"{{"durations":{{"checking_idle_s":{duration}}}}}"#
        )));
    }
    j.together_failure(Regime::R6, 0, &vec![-30; count]);
    j.start_hunt();
    j
}

/// Records the next decision, an unattributed failure.
pub(crate) fn attribute(j: &mut Journal) -> Seq {
    let decision = j.next_decision();
    assert!(
        matches!(&decision.payload, DecisionPayload::Failure(f) if f.attribution == Attribution::Unattributed),
        "{decision:?}"
    );
    j.decide(decision)
}

/// Runs the next trial, a parked hunt group, failing it when `fail`.
pub(crate) fn run_group(j: &mut Journal, fail: bool) {
    let plan = j.next_trial();
    assert_eq!(plan.condition, Condition::Parked, "{plan:?}");
    let outcome = if fail {
        Outcome::Failure
    } else {
        Outcome::Pass
    };
    j.trial(&plan, outcome);
    if fail {
        attribute(j);
    }
}

/// Appends hunt `hunt` for a `regime` failure loading `loaded` at `failing`.
pub(crate) fn start_regime_hunt(
    j: &mut Journal,
    hunt: u32,
    regime: Regime,
    loaded: &[u8],
    failing: &[i64],
) -> Seq {
    let count = failing.len();
    let start = HuntStart {
        hunt: NonZeroU32::new(hunt).unwrap(),
        failure: Seq::new(1).unwrap(),
        trial: None,
        regime,
        workload: workloads(regime)[0].id(),
        cores: set(loaded),
        duration_s: SHORT,
        failing: j.profile(failing).offsets(),
        parked: j.profile(&vec![0; count]).offsets(),
        parked_seq: None,
        candidates: set(&ids(count)),
        trials: 5,
        trial_s: SHORT,
        miss: 0.0,
        rate: 0.0,
        ranking: ids(count).into_iter().map(core).collect(),
        reason: None,
    };
    j.append(start.into(), &[])
}

pub(crate) fn start_hunt_at(j: &mut Journal, hunt: u32, loaded: &[u8], failing: &[i64]) -> Seq {
    start_regime_hunt(j, hunt, Regime::R6, loaded, failing)
}

pub(crate) fn cancel_hunt(j: &mut Journal, hunt: u32) {
    let end = HuntEnd {
        hunt: NonZeroU32::new(hunt).unwrap(),
        result: HuntOutcome::Cancelled,
        cores: set(&[]),
        members: Vec::new(),
        groups: 0,
        reason: "test".to_owned(),
    };
    j.append(end.into(), &[]);
}

pub(crate) fn group_of(action: &Action) -> Option<&HuntGroup> {
    if let Action::Decide(decision) = action
        && let DecisionPayload::HuntGroup(group) = &decision.payload
    {
        Some(group)
    } else {
        None
    }
}

pub(crate) fn at(plan: &TrialPlan, id: u8) -> i64 {
    plan.profile.get(core(id)).unwrap().get()
}

/// Sixteen cores at their -10 limit under an R6-only cycle, for the repeated parked-core probe.
pub(crate) fn repeated_session() -> Journal {
    let count = 16;
    let mut j = Journal::new(&ids(count), &vec![Start::at_limit(-10).fail(-11); count]);
    j.profile_change(&vec![-10; count]);
    j.ranking(&ids(count));
    j.load(&config(
        r#"{"checking":{"cycle":["R6"]},"durations":{"checking_idle_s":120}}"#,
    ));
    j
}

/// Core 03 fails below -8, or below 0 while core 04 is below -9.
pub(crate) fn repeated_core_oracle(plan: &TrialPlan) -> Outcome {
    if at(plan, 3) < -8 || (at(plan, 3) < 0 && at(plan, 4) < -9) {
        Outcome::Failure
    } else {
        Outcome::Pass
    }
}

pub(crate) fn members_of(pairs: &[(u8, i64)]) -> Vec<Member> {
    pairs
        .iter()
        .map(|&(id, offset)| Member {
            core: core(id),
            offset: off(offset),
        })
        .collect()
}

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
        let action = self.next();
        let Action::Decide(decision) = action else {
            panic!("expected a hunt start, got {action:?}");
        };
        let DecisionPayload::HuntStart(start) = decision.payload else {
            panic!("expected a hunt start, got {:?}", decision.payload);
        };
        (start, decision.cause)
    }

    pub(super) fn start_hunt(&mut self) -> (Seq, HuntStart) {
        let (start, cause) = self.hunt_start_payload();
        let seq = self.append(start.clone().into(), &cause);
        (seq, start)
    }

    pub(super) fn group_plan(&self) -> (HuntGroup, Vec<Seq>) {
        let action = self.next();
        let Action::Decide(decision) = action else {
            panic!("expected a hunt group, got {action:?}");
        };
        let DecisionPayload::HuntGroup(group) = decision.payload else {
            panic!("expected a hunt group, got {:?}", decision.payload);
        };
        (group, decision.cause)
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
