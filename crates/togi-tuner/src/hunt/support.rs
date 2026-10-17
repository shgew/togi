use std::num::NonZeroU32;

use togi_journal::{
    Attribution, CorePhase, CoreTransition, HuntEnd, HuntGroup, HuntOutcome, HuntStage, HuntStart,
    Inference, Member, Outcome, Seq,
};
use togi_machine::{Condition, Regime, workloads};

use super::plan::GroupOutcome;
use crate::testkit::{Journal, Start, config, set};
use crate::tests::{core, off};
use crate::{Action, DecisionAction, DecisionPayload, Load, TrialPlan};

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
    let all = ids(count);
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&all)),
        Condition::Together,
        &vec![-30; count],
        duration,
    );
    j.trial(&plan, Outcome::Failure);
    attribute(&mut j);
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

/// Runs hunt `hunt` up to its first `stage` group, runs that group `trials` times with
/// `outcome`, then cancels the hunt.
pub(crate) fn ran_group(
    j: &mut Journal,
    hunt: u32,
    loaded: &[u8],
    failing: &[i64],
    stage: HuntStage,
    outcome: Outcome,
    trials: usize,
) {
    start_hunt_at(j, hunt, loaded, failing);
    let planned = reach(j, stage);
    j.decide(planned);
    for _ in 0..trials {
        run_group(j, outcome == Outcome::Failure);
    }
    cancel_hunt(j, hunt);
}

/// Passes every trial until the open hunt plans its first `stage` group, and returns that plan.
pub(crate) fn reach(j: &mut Journal, stage: HuntStage) -> DecisionAction {
    let index = u32::from(stage == HuntStage::Complement);
    for _ in 0..200 {
        let action = j.next();
        if group_of(&action).is_some_and(|group| group.stage == stage && group.index == index) {
            let Action::Decide(decision) = action else {
                unreachable!();
            };
            return decision;
        }
        j.step(&mut |_: &TrialPlan| Outcome::Pass);
    }
    panic!("the hunt never planned a {stage:?} group");
}

/// Moves core `id` back from search to has-room at `offset`.
pub(crate) fn restore_has_room(j: &mut Journal, id: u8, offset: i64) {
    j.core_phase(CoreTransition {
        core: core(id),
        from: Some(CorePhase::Search),
        to: CorePhase::HasRoom,
        offset: off(offset),
        pass: None,
        failure_point: None,
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    });
}

/// An active-hunt projection scenario: hunt 2's one group over cores 00 and 01 of four, all at
/// -30, after `prior` passes of hunt 1's part group, an optional core-00 reset, and the group's
/// `live` trials, failing where true. The rest is what the projections must show.
pub(crate) struct ProjectionCase {
    pub(crate) name: &'static str,
    pub(crate) stage: HuntStage,
    prior: usize,
    live: Vec<bool>,
    inferred: Option<Inference>,
    skipped: bool,
    reset: bool,
    escalated: bool,
    pub(crate) probe: Option<Member>,
    pub(crate) held: Vec<Member>,
    pub(crate) passes: u32,
    pub(crate) outcome: Option<GroupOutcome>,
}

impl ProjectionCase {
    /// Journals the scenario and returns the journal with the group's seq.
    pub(crate) fn journal(&self) -> (Journal, Seq) {
        let mut j = Journal::new(&ids(4), &[Start::has_room(-30); 4]);
        j.profile_change(&[-30; 4]);
        j.ranking(&ids(4));
        if self.prior > 0 {
            ran_group(
                &mut j,
                1,
                &ids(4),
                &[-30; 4],
                HuntStage::Part,
                Outcome::Pass,
                self.prior,
            );
        }
        if self.reset {
            j.reset(0);
            let transition = j.next_decision();
            assert!(
                matches!(&transition.payload, DecisionPayload::CorePhase(_)),
                "{}",
                self.name
            );
            j.decide(transition);
            restore_has_room(&mut j, 0, -30);
        }
        let begin = start_hunt_at(&mut j, 2, &ids(4), &[-30; 4]);
        let group = HuntGroup {
            hunt: NonZeroU32::new(2).unwrap(),
            group: NonZeroU32::MIN,
            cores: set(&[0, 1]),
            profile: j.profile(&[-30, -30, 0, 0]).offsets(),
            set: set(&ids(4)),
            granularity: 2,
            stage: self.stage,
            index: 0,
            duration_s: SHORT,
            escalated: self.escalated,
            full_checked: false,
            any_failed: false,
            probe: self.probe,
            held: self.held.clone(),
            inferred: self.inferred,
            skipped: self.skipped,
            reason: Some("test".to_owned()),
        };
        let seq = j.append(group.into(), &[begin]);
        for &fail in &self.live {
            run_group(&mut j, fail);
        }
        (j, seq)
    }
}

/// The scenarios the dashboard's hunt plan and the hunt's group view must both show.
pub(crate) fn projection_cases() -> Vec<ProjectionCase> {
    let base = || ProjectionCase {
        name: "",
        stage: HuntStage::Part,
        prior: 0,
        live: Vec::new(),
        inferred: None,
        skipped: false,
        reset: false,
        escalated: false,
        probe: None,
        held: Vec::new(),
        passes: 0,
        outcome: None,
    };
    vec![
        ProjectionCase {
            name: "running reuses earlier valid passes",
            prior: 2,
            live: vec![false],
            passes: 3,
            ..base()
        },
        ProjectionCase {
            name: "passing complement accumulates trials",
            stage: HuntStage::Complement,
            prior: 4,
            live: vec![false],
            passes: 5,
            outcome: Some(GroupOutcome::Passed),
            ..base()
        },
        ProjectionCase {
            name: "failure invalidates earlier passes",
            prior: 2,
            live: vec![true],
            outcome: Some(GroupOutcome::Failed),
            ..base()
        },
        ProjectionCase {
            name: "inferred pass",
            prior: 5,
            inferred: Some(Inference::Pass),
            passes: 5,
            outcome: Some(GroupOutcome::Passed),
            ..base()
        },
        ProjectionCase {
            name: "inferred failure",
            stage: HuntStage::Complement,
            inferred: Some(Inference::Failure),
            outcome: Some(GroupOutcome::Failed),
            ..base()
        },
        ProjectionCase {
            name: "skipped",
            skipped: true,
            outcome: Some(GroupOutcome::Skipped),
            ..base()
        },
        ProjectionCase {
            name: "reset excludes reused evidence",
            prior: 4,
            reset: true,
            live: vec![false],
            passes: 1,
            ..base()
        },
        ProjectionCase {
            name: "escalated full uses only new trials",
            stage: HuntStage::Full,
            prior: 4,
            live: vec![false],
            escalated: true,
            passes: 1,
            ..base()
        },
        ProjectionCase {
            name: "member probe preserves held members",
            stage: HuntStage::Probe,
            prior: 4,
            live: vec![false],
            probe: Some(Member {
                core: core(0),
                offset: off(-30),
            }),
            held: members_of(&[(1, -30)]),
            passes: 1,
            ..base()
        },
    ]
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
    let phase = core.phase.wire().expect("the core has a phase");
    journal.core_phase(leave(phase, CorePhase::Search));
    journal.profile_change(checked);
    journal.cycle_start(1, &[]);
    let end = journal.cycle_end(1, true, true);
    journal.core_phase(leave(CorePhase::Search, phase));
    journal.profile_change(back_to);
    end
}
