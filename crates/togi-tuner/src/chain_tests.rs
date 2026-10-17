use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU32;

use togi_journal::{
    CheckingChain, CheckingStep, CorePhase, CycleEvent, Decision, Outcome, Payload, Rederived, Seq,
    SessionStart,
};
use togi_machine::{CcdId, Condition, CoreInfo, CoreSet, Regime, Signal, WorkloadId};

use crate::schedule_tests::{decision, transition};
use crate::testkit::{self, Journal, R7Report, Start, fresh, set};
use crate::tests::core;
use crate::{
    Action, ClassKey, DecisionAction, DecisionPayload, FoldError, Load, PartNo, Secs, StepNo,
    TopologyError, TrialContext, TrialPlan, Window,
};

const STARTS: [i64; 8] = [-10, -20, -30, -40, -20, -20, -20, -20];
const SHORT: u32 = 120;
const FULL_REQUESTS: [(u8, f64); 4] = [(0, 1.19), (1, 1.11), (2, 1.2), (3, 1.09)];
const PARTIAL_REQUESTS: [(u8, f64); 3] = [(0, 1.19), (1, 1.25), (3, 1.1)];

fn grouped(ccds: &[&[u8]]) -> Vec<CoreInfo> {
    ccds.iter()
        .zip(0_u8..)
        .flat_map(|(ids, ccd)| {
            ids.iter().map(move |&id| CoreInfo {
                core: core(id),
                ccd: CcdId::try_from(ccd).unwrap(),
                cpus: Vec::new(),
            })
        })
        .collect()
}

fn two_ccds() -> Vec<CoreInfo> {
    grouped(&[&[0, 1, 2, 3], &[4, 5, 6, 7]])
}

fn one_ccd() -> Vec<CoreInfo> {
    grouped(&[&[0, 1, 2, 3, 4, 5, 6, 7]])
}

fn payload(action: &Action) -> &DecisionPayload {
    let Action::Decide(decision) = action else {
        panic!("expected a decision, the tuner asked for {action:?}");
    };
    &decision.payload
}

fn decided(action: Action) -> DecisionAction {
    let Action::Decide(decision) = action else {
        panic!("expected a decision, the tuner asked for {action:?}");
    };
    decision
}

fn chain_of(action: &Action) -> &CheckingChain {
    let payload = payload(action);
    let DecisionPayload::CheckingChain(chain) = payload else {
        panic!("expected a checking.chain, got {payload:?}");
    };
    chain
}

fn trial_of(action: &Action) -> &TrialPlan {
    let Action::RunTrial(plan) = action else {
        panic!("expected a trial, the tuner asked for {action:?}");
    };
    plan
}

fn cycle_end(action: &Action) -> Option<bool> {
    if let Action::Decide(DecisionAction {
        payload: DecisionPayload::CheckingCycle(cycle),
        ..
    }) = action
        && cycle.event == CycleEvent::End
    {
        Some(cycle.passed)
    } else {
        None
    }
}

/// A session over `cores` with every core has-room at `offsets`, its checking profile set and,
/// under `cycle` (the default cycle when `None`), the first cycle open with its first R7 step
/// begun.
fn journal(cores: Vec<CoreInfo>, offsets: &[i64], cycle: Option<&str>) -> Journal {
    seeded_journal(cores, offsets, cycle, |_| {})
}

/// As `journal`, with `seed` appending its records before the cycle opens.
fn seeded_journal(
    cores: Vec<CoreInfo>,
    offsets: &[i64],
    cycle: Option<&str>,
    seed: impl FnOnce(&mut Journal),
) -> Journal {
    let ids: Vec<u8> = cores.iter().map(|info| info.core.get()).collect();
    let starts: Vec<Start> = offsets
        .iter()
        .map(|&offset| Start::has_room(offset))
        .collect();
    let mut j = Journal::on(&ids, cores, &starts);
    j.ranking(&ids);
    if let Some(cycle) = cycle {
        j.load(&testkit::config(cycle));
    }
    seed(&mut j);
    for _ in 0..3 {
        let next = j.next_decision();
        assert!(
            matches!(
                next.payload,
                DecisionPayload::ProfileChange(_)
                    | DecisionPayload::CheckingCycle(_)
                    | DecisionPayload::CheckingStep(_)
            ),
            "{next:?}"
        );
        j.decide(next);
    }
    assert!(j.run().checking.open().is_some());
    j
}

fn r7_only() -> &'static str {
    r#"{"checking":{"cycle":["R7"]}}"#
}

pub(crate) fn chain_journal() -> Journal {
    journal(two_ccds(), &STARTS, Some(r7_only()))
}

fn run(
    j: &mut Journal,
    plan: &TrialPlan,
    outcome: Outcome,
    requests: &[(u8, f64)],
    named: Option<u8>,
) -> Seq {
    let (intent, trial) = j.intent(plan);
    let signal = (outcome == Outcome::Failure).then_some(Signal::ComputationError);
    let mut end = Journal::end_payload(&trial, outcome, signal);
    end.duration_s = plan.duration.get();
    end.voltage_requests_v = requests
        .iter()
        .map(|&(id, volts)| (core(id), volts))
        .collect();
    end.core = named.map(core);
    j.append(end.into(), &[intent])
}

fn long_secs(j: &Journal, cores: CoreSet) -> u32 {
    let run = j.run();
    let whole = run.config.checking_all_core.get();
    let ccds = u32::try_from(run.topo.ccds().len()).unwrap();
    match ccds {
        1 => whole,
        _ if cores == run.topo.all() => whole - ccds * (whole / 4),
        _ => whole / 4,
    }
}

pub(crate) fn pass_part(j: &mut Journal, cores: &[u8], requests: &[(u8, f64)]) -> Seq {
    let cores = set(cores);
    let mut last = None;
    for trial in 0..4 {
        let plan = j.next_trial();
        let want = if trial == 3 {
            long_secs(j, cores)
        } else {
            SHORT
        };
        assert_eq!(plan.load, Load::Many(cores), "trial {trial}: {plan:?}");
        assert_eq!(plan.duration.get(), want, "trial {trial}: {plan:?}");
        assert!(
            matches!(plan.context, TrialContext::Cycle { step, .. } if step.get().get() == 1),
            "{plan:?}"
        );
        last = Some(run(j, &plan, Outcome::Pass, requests, None));
    }
    last.unwrap()
}

fn same_next(j: &Journal) -> Action {
    let live = j.next();
    assert_eq!(
        fresh(&j.events).next().ok().as_ref(),
        Some(&live),
        "a resumed session asks for something else"
    );
    live
}

fn values(j: &Journal) -> Vec<i64> {
    j.offsets().iter().map(|(_, offset)| offset.get()).collect()
}

fn checking_offsets(j: &Journal) -> togi_machine::PerCore<togi_machine::TargetOffset> {
    j.run().checking.profile().unwrap().offsets()
}

/// Moves core `id` `counts` shallower and records the checking profile change that follows.
fn shallower(j: &mut Journal, id: u8, counts: i64) {
    let to = j.offsets().get(core(id)).unwrap().get() + counts;
    j.core_phase(transition(id, CorePhase::HasRoom, to));
    let change = j.next_decision();
    assert!(
        matches!(change.payload, DecisionPayload::ProfileChange(_)),
        "{change:?}"
    );
    j.decide(change);
}

fn rederive(j: &mut Journal, part: u32, cores: &[u8], change: Rederived) -> CheckingChain {
    let action = same_next(j);
    let chain = chain_of(&action).clone();
    assert_eq!(
        (chain.part.get(), chain.cores, chain.rederived),
        (part, set(cores), Some(change)),
        "{chain:?}"
    );
    assert_eq!(chain.profile, checking_offsets(j));
    j.decide(decided(action));
    chain
}

fn r7_class(cores: &[u8], secs: u32) -> ClassKey {
    ClassKey::new(
        togi_machine::pick_workload(Regime::R7, 0).id(),
        set(cores),
        Secs::new(NonZeroU32::new(secs).unwrap()),
    )
}

fn required(j: &Journal, cores: &[u8]) -> bool {
    j.run()
        .checking
        .locate(r7_class(cores, SHORT), Some(0))
        .is_some()
}

/// The passes the open cycle's step `step` requires of the R7 class over `cores`.
fn needed(j: &Journal, step: u32, cores: &[u8], secs: u32) -> Option<u16> {
    let mut plan = j.manual(
        Regime::R7,
        0,
        Load::Many(set(cores)),
        Condition::Together,
        &values(j),
        secs,
    );
    plan.context = TrialContext::Cycle {
        cycle: j.run().checking.open().unwrap().number,
        step: StepNo::new(NonZeroU32::new(step).unwrap()),
        part: PartNo::new(NonZeroU32::MIN),
    };
    j.run()
        .cycle_retry_requirement(&plan)
        .map(|requirement| requirement.needed.get())
}

fn with_partial() -> Journal {
    let mut j = chain_journal();
    pass_part(&mut j, &[0, 1, 2, 3], &FULL_REQUESTS);
    let chain = j.next();
    assert_eq!(chain_of(&chain).cores, set(&[0, 1, 3]));
    j.decide(decided(chain));
    j
}

fn chain_record(j: &Journal, step: u32, part: u32, cores: &[u8]) -> Payload {
    CheckingChain {
        cycle: j.run().checking.open().unwrap().number,
        step: NonZeroU32::new(step).unwrap(),
        ccd: CcdId::new(0).unwrap(),
        workload: togi_machine::pick_workload(Regime::R7, 0).id(),
        groups: Vec::new(),
        cores: set(cores),
        source_seqs: Vec::new(),
        profile: checking_offsets(j),
        part: NonZeroU32::new(part).unwrap(),
        rederived: None,
    }
    .into()
}

fn step_record(j: &Journal, step: u32) -> Payload {
    CheckingStep {
        cycle: j.run().checking.open().unwrap().number,
        step: NonZeroU32::new(step).unwrap(),
        profile: checking_offsets(j),
    }
    .into()
}

#[test]
fn r7_chain_derives_from_completed_predecessor() {
    let mut j = chain_journal();
    let seq = pass_part(
        &mut j,
        &[0, 1, 2, 3],
        &[(0, 1.1), (1, 1.11), (2, 1.2), (3, 1.09)],
    );
    let action = j.next();
    let chain = chain_of(&action).clone();
    assert_eq!(chain.cores, set(&[0, 1, 3]));
    assert_eq!(chain.source_seqs, [seq]);
    assert_eq!(chain.groups[0], set(&[2]));
    assert_eq!(
        (chain.ccd, chain.part.get(), chain.rederived),
        (CcdId::new(0).unwrap(), 1, None)
    );
    j.decide(decided(action));
    assert_eq!(j.run().checking.open().unwrap().steps_done(), 0);

    let seq = pass_part(&mut j, &[0, 1, 3], &[(0, 1.12), (1, 1.25), (3, 1.1)]);
    let action = j.next();
    let chain = chain_of(&action).clone();
    assert_eq!((chain.cores, chain.part.get()), (set(&[0, 3]), 2));
    assert_eq!(chain.source_seqs, [seq]);
    j.decide(decided(action));

    pass_part(&mut j, &[0, 3], &[(0, 1.2), (3, 1.1)]);
    let action = j.next();
    let end = chain_of(&action).clone();
    assert!(end.cores.is_empty(), "two loaded cores must end the chain");
    assert_eq!((end.groups[0], end.part.get()), (set(&[0]), 3));
    assert!(!end.source_seqs.is_empty());
    j.decide(decided(action));

    pass_part(&mut j, &[4, 5, 6, 7], &[]);
    let action = j.next();
    let end = chain_of(&action).clone();
    assert_eq!(end.ccd, CcdId::new(1).unwrap());
    assert!(
        end.cores.is_empty(),
        "a tied offset fallback must end the chain"
    );
    assert!(end.source_seqs.is_empty());
    assert_eq!(end.groups, [set(&[4, 5, 6, 7])]);
    j.decide(decided(action));

    pass_part(&mut j, &[0, 1, 2, 3, 4, 5, 6, 7], &[]);
    assert_eq!(cycle_end(&j.next()), Some(true));
    j.assert_resumes();
}

#[test]
fn r7_chain_requires_all_predecessor_passes() {
    let mut j = chain_journal();
    for _ in 0..3 {
        let plan = j.next_trial();
        j.trial(&plan, Outcome::Pass);
    }
    let long = j.next_trial();
    assert_eq!(long.duration.get(), long_secs(&j, set(&[0, 1, 2, 3])));
    j.trial(&long, Outcome::Inconclusive);
    let retry = j.next_trial();
    assert!(retry.retry, "{retry:?}");
    assert_eq!((retry.load, retry.duration), (long.load, long.duration));
    j.trial(&retry, Outcome::Pass);
    chain_of(&j.next());
    j.assert_resumes();
}

#[test]
fn r7_chain_rederives_after_profile_change_unstarted_node() {
    let mut j = with_partial();
    shallower(&mut j, 0, 1);
    rederive(&mut j, 1, &[0, 1, 3], Rederived::Unchanged);
    let plan = trial_of(&same_next(&j)).clone();
    assert_eq!(plan.load, Load::Many(set(&[0, 1, 3])));
    j.assert_resumes();
}

#[test]
fn r7_chain_rederives_after_profile_change_inconclusive_set_unchanged() {
    let mut j = with_partial();
    let first = j.next_trial();
    j.trial(&first, Outcome::Inconclusive);
    shallower(&mut j, 0, 1);
    rederive(&mut j, 1, &[0, 1, 3], Rederived::Unchanged);
    let plan = trial_of(&same_next(&j)).clone();
    assert!(
        !plan.retry,
        "the retry survived the re-derivation: {plan:?}"
    );
    assert_eq!(plan.load, Load::Many(set(&[0, 1, 3])));
    assert_eq!(plan.duration.get(), SHORT);
    j.assert_resumes();
}

#[test]
fn r7_chain_rederives_after_profile_change_inconclusive_set_changed() {
    let mut j = with_partial();
    let first = j.next_trial();
    j.trial(&first, Outcome::Inconclusive);
    shallower(&mut j, 0, 10);
    rederive(&mut j, 1, &[1, 2, 3], Rederived::Changed);
    let plan = trial_of(&same_next(&j)).clone();
    assert!(!plan.retry, "{plan:?}");
    assert_eq!(plan.load, Load::Many(set(&[1, 2, 3])));
    j.assert_resumes();
}

#[test]
fn r7_chain_rederives_after_profile_change_passed_set_unchanged() {
    let mut j = with_partial();
    pass_part(&mut j, &[0, 1, 3], &PARTIAL_REQUESTS);
    shallower(&mut j, 0, 1);
    rederive(&mut j, 1, &[0, 1, 3], Rederived::Unchanged);
    let next = same_next(&j);
    let chain = chain_of(&next);
    assert_eq!((chain.part.get(), chain.cores), (2, set(&[0, 3])));
    j.assert_resumes();
}

#[test]
fn r7_chain_rederives_after_profile_change_passed_set_changed() {
    let mut j = with_partial();
    pass_part(&mut j, &[0, 1, 3], &PARTIAL_REQUESTS);
    shallower(&mut j, 0, 10);
    rederive(&mut j, 1, &[1, 2, 3], Rederived::Changed);
    pass_part(&mut j, &[1, 2, 3], &[]);
    let next = same_next(&j);
    assert_eq!(chain_of(&next).part.get(), 2);
    j.assert_resumes();
}

#[test]
fn r7_chain_rederives_after_profile_change_satisfied_parent_and_started_child() {
    let mut j = with_partial();
    j.append(chain_record(&j, 1, 2, &[0, 3]), &[]);
    let child = j.manual(
        Regime::R7,
        0,
        Load::Many(set(&[0, 3])),
        Condition::Together,
        &values(&j),
        SHORT,
    );
    j.trial(&child, Outcome::Inconclusive);
    assert!(j.run().retry.is_some());
    shallower(&mut j, 2, 1);
    assert!(
        !required(&j, &[0, 1, 3]),
        "the stale parent stayed required"
    );
    assert!(!required(&j, &[0, 3]), "the stale child stayed required");
    assert!(j.run().retry.is_none());
    rederive(&mut j, 1, &[0, 1, 3], Rederived::Unchanged);
    assert!(required(&j, &[0, 1, 3]));
    assert!(
        !required(&j, &[0, 3]),
        "the child is required before its own re-derivation"
    );
    j.assert_resumes();
}

#[test]
fn r7_chain_rederives_after_profile_change_on_the_other_ccd_only() {
    let mut j = with_partial();
    pass_part(&mut j, &[0, 1, 3], &PARTIAL_REQUESTS);
    j.decide(decided(j.next()));
    pass_part(&mut j, &[0, 3], &[(0, 1.2), (3, 1.1)]);
    j.decide(decided(j.next()));
    shallower(&mut j, 4, 1);
    rederive(&mut j, 1, &[0, 1, 3], Rederived::Unchanged);
    rederive(&mut j, 2, &[0, 3], Rederived::Unchanged);
    rederive(&mut j, 3, &[], Rederived::Unchanged);
    let plan = trial_of(&same_next(&j)).clone();
    assert!(
        !plan.load.cores().contains(core(0)),
        "the unchanged CCD ran a new trial: {plan:?}"
    );
    j.assert_resumes();
}

#[test]
fn r7_chain_rederives_after_profile_change_several_backoffs_before_revisiting() {
    let mut j = with_partial();
    let before = j.events.len();
    for id in [0, 1, 3] {
        shallower(&mut j, id, 1);
    }
    rederive(&mut j, 1, &[0, 1, 3], Rederived::Unchanged);
    let chains = j.events[before..]
        .iter()
        .filter(|event| matches!(event.payload, Payload::CheckingChain(_)))
        .count();
    assert_eq!(chains, 1);
    j.assert_resumes();
}

#[test]
fn r7_chain_rederives_after_profile_change_one_ccd() {
    let mut j = journal(one_ccd(), &STARTS, Some(r7_only()));
    pass_part(&mut j, &[0, 1, 2, 3, 4, 5, 6, 7], &[]);
    j.decide(decided(j.next()));
    shallower(&mut j, 0, 1);
    rederive(&mut j, 1, &[1, 2, 3, 4, 5, 6, 7], Rederived::Unchanged);
    for _ in 0..100 {
        let action = same_next(&j);
        if let Some(passed) = cycle_end(&action) {
            assert!(passed);
            j.assert_resumes();
            return;
        }
        match action {
            Action::RunTrial(plan) => {
                assert_ne!(plan.load.cores().len(), 8, "the full part re-ran: {plan:?}");
                j.trial(&plan, Outcome::Pass);
            }
            other @ (Action::Decide(_) | Action::ReadRanking) => {
                j.decide(decided(other));
            }
        }
    }
    panic!("the one-CCD chain did not finish");
}

#[test]
fn r7_chain_rederives_after_profile_change_repeated_step_of_one_workload() {
    let mut j = journal(
        two_ccds(),
        &STARTS,
        Some(r#"{"checking":{"cycle":["R7","R7","R7","R7"]}}"#),
    );
    assert_eq!(
        togi_machine::pick_workload(Regime::R7, 3).id(),
        togi_machine::pick_workload(Regime::R7, 0).id()
    );
    j.append(step_record(&j, 4), &[]);
    for step in [1, 4] {
        j.append(chain_record(&j, step, 1, &[1, 2, 3]), &[]);
    }
    shallower(&mut j, 0, 1);
    for step in [1, 4] {
        j.append(chain_record(&j, step, 1, &[1, 2, 3]), &[]);
    }
    assert_eq!(needed(&j, 4, &[1, 2, 3], SHORT), Some(6));
    assert_eq!(needed(&j, 4, &[1, 2, 3], 300), Some(2));
    assert_eq!(needed(&j, 1, &[1, 2, 3], SHORT), Some(3));
    j.assert_resumes();
}

#[test]
fn r7_chain_reruns_a_failed_class_that_left_the_chain() {
    let mut j = chain_journal();
    pass_part(
        &mut j,
        &[0, 1, 2, 3],
        &[(0, 1.1985), (1, 1.11), (2, 1.2), (3, 1.09)],
    );
    j.decide(decided(j.next()));
    let old = set(&[0, 1, 3]);
    let partial = j.next_trial();
    assert_eq!(partial.load, Load::Many(old));
    run(&mut j, &partial, Outcome::Failure, &[], Some(0));
    let mut action = j.next();
    for _ in 0..6 {
        if matches!(action, Action::RunTrial(_)) {
            break;
        }
        j.decide(decided(action));
        action = j.next();
    }
    assert_eq!(j.offsets().get(core(0)).unwrap().get(), -9);
    for _ in 0..j.run().config.needed.get() {
        let plan = trial_of(&action).clone();
        assert!(
            matches!(plan.context, TrialContext::Rerun { .. }),
            "{plan:?}"
        );
        assert_eq!(plan.load, Load::Many(old));
        j.trial(&plan, Outcome::Pass);
        action = j.next();
    }
    assert_eq!(j.run().cycle_action(), Some(action));
    rederive(&mut j, 1, &[1, 2, 3], Rederived::Changed);
    assert!(!required(&j, &[0, 1, 3]));
    j.assert_resumes();
}

const REORDER: [f64; 8] = [1.185, 1.180, 1.170, 1.160, 1.250, 1.190, 1.215, 1.150];

fn reorder_requests(plan: &TrialPlan) -> Vec<(u8, f64)> {
    let cores = plan.load.cores();
    if plan.regime != Regime::R7 || cores.iter().any(|core| core.get() > 7) {
        return Vec::new();
    }
    let shift = plan.profile.get(core(0)).unwrap().get() + 24;
    cores
        .iter()
        .map(|core| {
            let id = core.get();
            let mut volts = REORDER[usize::from(id)];
            if id == 0 {
                volts += f64::from(i32::try_from(shift).unwrap()) * togi_requests::VOLTS_PER_COUNT;
            }
            (id, volts)
        })
        .collect()
}

fn reorder_step(j: &mut Journal, action: Action) {
    match action {
        Action::RunTrial(plan) => {
            let requests = reorder_requests(&plan);
            run(j, &plan, Outcome::Pass, &requests, None);
        }
        other @ (Action::Decide(_) | Action::ReadRanking) => {
            j.decide(decided(other));
        }
    }
}

#[test]
fn r7_chain_retests_reordered_part_after_backoff() {
    let ccd0: Vec<u8> = (0..8).collect();
    let ccd1: Vec<u8> = (8..16).collect();
    let offsets: Vec<i64> = (0..16).map(|id| if id < 8 { -24 } else { -20 }).collect();
    let mut j = journal(
        grouped(&[&ccd0, &ccd1]),
        &offsets,
        Some(r#"{"checking":{"cycle":["R7","R1"]}}"#),
    );
    let riskiest = set(&[1, 2, 3, 5, 7]);

    for _ in 0..400 {
        let action = j.next();
        if matches!(&action, Action::RunTrial(plan) if plan.regime == Regime::R1) {
            break;
        }
        reorder_step(&mut j, action);
    }
    assert!(
        !j.events.iter().any(
            |event| matches!(&event.payload, Payload::TrialIntent(intent) if intent.cores == riskiest)
        ),
        "the riskiest load ran before the backoffs"
    );
    let first_backoff = j.events.len();

    let mut failures = 0;
    let mut offsets0 = Vec::new();
    let mut finished = false;
    for _ in 0..1500 {
        let action = if failures > 0 {
            same_next(&j)
        } else {
            j.next()
        };
        if let Some(passed) = cycle_end(&action) {
            assert!(passed);
            assert!(
                !required(&j, &[0, 1, 2, 3, 7]),
                "the old-order part is still required"
            );
            j.decide(decided(action));
            finished = true;
            break;
        }
        if let Action::RunTrial(plan) = &action
            && plan.regime == Regime::R1
            && !plan.retry
            && plan.load == Load::Single(core(0))
            && failures < 3
        {
            run(&mut j, plan, Outcome::Failure, &[], Some(0));
            failures += 1;
            continue;
        }
        reorder_step(&mut j, action);
        if let Payload::ProfileChange(change) = &j.events.last().unwrap().payload {
            offsets0.push(change.to.get(core(0)).unwrap().get());
        }
    }
    assert!(finished, "the cycle did not end");
    assert_eq!(offsets0, [-23, -22, -21]);

    let profile = checking_offsets(&j);
    let mut derived: BTreeMap<u8, Vec<(u32, CoreSet, Option<Rederived>)>> = BTreeMap::new();
    let mut trials = Vec::new();
    let (mut short, mut long) = (0, 0);
    let later = &j.events[first_backoff..];
    for (at, event) in later.iter().enumerate() {
        if let Payload::CheckingChain(chain) = &event.payload {
            assert_eq!(chain.profile, profile, "a chain record at a stale profile");
            derived.entry(chain.ccd.get()).or_default().push((
                chain.part.get(),
                chain.cores,
                chain.rederived,
            ));
        } else if let Payload::TrialIntent(intent) = &event.payload
            && intent.regime == Regime::R7
        {
            trials.push(intent.cores);
            if intent.cores == riskiest {
                if intent.duration_s == SHORT {
                    short += 1;
                } else {
                    long += 1;
                }
            }
        } else if let Payload::ProfileChange(_) = &event.payload {
            let rerun = later[at + 1..].iter().find_map(|event| {
                if let Payload::TrialIntent(intent) = &event.payload {
                    Some(intent.rerun)
                } else {
                    None
                }
            });
            assert_eq!(
                rerun,
                Some(true),
                "a backoff was not followed by its class's rerun"
            );
        }
    }
    let unchanged = Some(Rederived::Unchanged);
    assert_eq!(
        derived[&0],
        [
            (1, set(&[0, 1, 2, 3, 5, 6, 7]), unchanged),
            (2, set(&[0, 1, 2, 3, 5, 7]), unchanged),
            (3, riskiest, Some(Rederived::Changed)),
            (4, set(&[1, 2, 3, 7]), unchanged),
            (5, set(&[2, 3, 7]), unchanged),
            (6, set(&[3, 7]), unchanged),
            (7, CoreSet::default(), unchanged),
        ]
    );
    assert_eq!(derived[&1], [(1, CoreSet::default(), unchanged)]);
    assert_eq!(trials, [riskiest; 4]);
    assert_eq!((short, long), (3, 1));
    j.assert_resumes();
}

#[test]
fn partial_passes_are_ordinary_evidence() {
    for carried in [false, true] {
        let mut j = chain_journal();
        pass_part(&mut j, &[0, 1, 2, 3], &[]);
        let chain = j.next();
        assert_eq!(chain_of(&chain).cores, set(&[1, 2, 3]));
        j.decide(decided(chain));
        let plan = j.next_trial();
        let class = plan.requirement.class;
        assert_eq!(class, r7_class(&[1, 2, 3], SHORT));
        let n = j.run().config.needed;
        for _ in 0..n.get() {
            if carried {
                j.carry(
                    "20230101T000000Z",
                    (Regime::R7, 0, &[1, 2, 3], SHORT),
                    Condition::Together,
                    &values(&j),
                    Outcome::Pass,
                );
            } else {
                j.trial(&plan, Outcome::Pass);
            }
        }
        let start = j.run().checking.open().unwrap().start;
        let profile = j.run().checking.profile().unwrap();
        let count = |window| {
            j.run()
                .evidence
                .count(class, &profile, window, n)
                .unwrap()
                .valid
        };
        let all = u32::from(n.get());
        assert_eq!(count(Window::LiveOrCarried { since: None }), all);
        let cycle = if carried { 0 } else { all };
        assert_eq!(
            count(Window::Live { since: Some(start) }),
            cycle,
            "carried {carried}"
        );
        let next = j.next_trial();
        assert_eq!(next.load, plan.load);
        let want = if carried { SHORT } else { 300 };
        assert_eq!(next.duration.get(), want, "carried {carried}");
        j.assert_resumes();
    }
}

#[test]
fn r7_full_cycle_includes_every_partial() {
    let mut j = journal(two_ccds(), &[-10, -20, -30, -40, -10, -20, -30, -40], None);
    let mut partials = 0;
    for _ in 0..1000 {
        let action = j.next();
        if let Some(passed) = cycle_end(&action) {
            let DecisionPayload::CheckingCycle(end) = payload(&action) else {
                unreachable!()
            };
            assert!(passed && end.full, "{end:?}");
            assert_eq!(partials, 48);
            j.decide(decided(action));
            j.assert_resumes();
            return;
        }
        match action {
            Action::RunTrial(plan) => {
                if plan.regime == Regime::R7 && plan.load.cores().len() < 4 {
                    partials += 1;
                }
                j.trial(&plan, Outcome::Pass);
            }
            other @ (Action::Decide(_) | Action::ReadRanking) => {
                j.decide(decided(other));
            }
        }
    }
    panic!("the cycle did not finish");
}

#[test]
fn r7_one_ccd_runs_full_then_chain_once() {
    let mut j = journal(one_ccd(), &STARTS, Some(r7_only()));
    pass_part(&mut j, &[0, 1, 2, 3, 4, 5, 6, 7], &[]);
    let action = j.next();
    assert_eq!(chain_of(&action).cores, set(&[1, 2, 3, 4, 5, 6, 7]));
    assert!(chain_of(&action).source_seqs.is_empty());
    j.decide(decided(action));
    for _ in 0..100 {
        let action = j.next();
        if let Some(passed) = cycle_end(&action) {
            assert!(passed);
            j.assert_resumes();
            return;
        }
        match action {
            Action::RunTrial(plan) => {
                assert_ne!(plan.load.cores().len(), 8, "the full part ran twice");
                j.trial(&plan, Outcome::Pass);
            }
            other @ (Action::Decide(_) | Action::ReadRanking) => {
                j.decide(decided(other));
            }
        }
    }
    panic!("the one-CCD chain did not finish");
}

#[test]
fn r7_chain_idles_request_ties_together() {
    let mut j = chain_journal();
    let tied = 1.2 - togi_requests::TIE_V / 2.0;
    pass_part(
        &mut j,
        &[0, 1, 2, 3],
        &[(0, 1.1), (1, tied), (2, 1.2), (3, 1.09)],
    );
    let action = j.next();
    let chain = chain_of(&action);
    assert_eq!(chain.groups[0], set(&[1, 2]));
    assert_eq!(chain.cores, set(&[0, 3]));
    j.assert_resumes();
}

#[test]
fn r7_partial_failure_does_not_complete_its_requirement() {
    let mut j = chain_journal();
    pass_part(&mut j, &[0, 1, 2, 3], &[]);
    j.decide(decided(j.next()));
    let plan = j.next_trial();
    let named = plan.load.cores().first().unwrap().get();
    run(&mut j, &plan, Outcome::Failure, &[], Some(named));
    let awaiting = j
        .run()
        .flight
        .awaiting
        .as_ref()
        .expect("awaits attribution");
    assert_eq!(awaiting.class, plan.requirement.class);
    let next = j.run().cycle_action().unwrap();
    let again = trial_of(&next);
    assert_eq!((again.load, again.duration), (plan.load, plan.duration));
    let failure = j.next_decision();
    assert!(
        matches!(failure.payload, DecisionPayload::Failure(_)),
        "{failure:?}"
    );
    let seq = j.decide(failure);
    let pending = j.run().failures.pending(seq).expect("a pending failure");
    assert_eq!(pending.class, plan.requirement.class);
    let next = j.run().cycle_action().unwrap();
    let again = trial_of(&next);
    assert_eq!((again.load, again.duration), (plan.load, plan.duration));
    j.assert_resumes();
}

#[test]
fn repeated_r7_partial_classes_add_trials() {
    let mut j = journal(
        two_ccds(),
        &STARTS,
        Some(r#"{"checking":{"cycle":["R7","R7","R7","R7"]}}"#),
    );
    for step in [1, 4] {
        if step != 1 {
            j.append(step_record(&j, step), &[]);
        }
        j.append(chain_record(&j, step, 1, &[1, 2, 3]), &[]);
    }
    assert_eq!(needed(&j, 4, &[1, 2, 3], SHORT), Some(6));
    assert_eq!(needed(&j, 4, &[1, 2, 3], 300), Some(2));
    assert_eq!(needed(&j, 1, &[1, 2, 3], SHORT), Some(3));
    assert_eq!(needed(&j, 1, &[1, 2, 3], 300), Some(1));
    j.assert_resumes();
}

#[test]
fn chain_rederives_after_a_backoff_before_its_step_counts() {
    let full = [(0, 1.19), (1, 1.11), (2, 1.2), (3, 1.195)];
    let partial = [(0, 1.19), (1, 1.25), (3, 1.195)];
    // Two counts make core 3 the full CCD's top requester, including under the named cap.
    let mut j = seeded_journal(two_ccds(), &STARTS, Some(r7_only()), |j| {
        for _ in 0..j.run().config.needed.get() {
            j.r7_fact(
                true,
                &[0, 1, 2, 3],
                &STARTS,
                &R7Report::default()
                    .requests(&full)
                    .top(&[2])
                    .clocks(&[(0, 5000)]),
            );
        }
    });
    pass_part(&mut j, &[0, 1, 2, 3], &full);
    let first = j.next();
    assert_eq!(chain_of(&first).cores, set(&[0, 1, 3]));
    j.decide(decided(first));
    pass_part(&mut j, &[0, 1, 3], &partial);
    let chain = j.next();
    assert_eq!(chain_of(&chain).cores, set(&[0, 3]));
    j.decide(decided(chain));
    let plan = j.next_trial();
    assert_eq!(plan.load, Load::Many(set(&[0, 3])));
    j.r7_trial(
        &[0, 3],
        &STARTS,
        Outcome::Failure,
        &R7Report::default()
            .requests(&[(0, 1.19), (3, 1.195)])
            .named(3)
            .clocks(&[(0, 5000)]),
    );
    let failure = j.next_decision();
    assert!(
        matches!(failure.payload, DecisionPayload::Failure(_)),
        "{failure:?}"
    );
    let failed = j.decide(failure);
    let backoff = j.next_decision();
    let moved = decision(&backoff);
    assert_eq!(moved.decision, Decision::Backoff);
    assert_eq!(moved.core, core(3));
    assert_eq!(moved.to_offset.get(), -38, "{moved:?}");
    j.decide(backoff);
    let change = j.next_decision();
    assert!(
        matches!(change.payload, DecisionPayload::ProfileChange(_)),
        "{change:?}"
    );
    j.decide(change);
    assert!(!j.run().reruns.is_empty());

    let mut action = j.next();
    while let Action::RunTrial(plan) = &action {
        assert_eq!(
            plan.context,
            TrialContext::Rerun { failure: failed },
            "a trial ran before the stale chain was re-derived: {plan:?}"
        );
        j.trial(plan, Outcome::Pass);
        action = j.next();
    }
    assert!(j.run().reruns.is_empty());
    j.assert_resumes();
    assert!(!required(&j, &[0, 1, 3]), "the stale part stayed required");
    assert!(!required(&j, &[0, 3]), "the stale part stayed required");
    let next = same_next(&j);
    let first = chain_of(&next).clone();
    assert_eq!(
        (first.part.get(), first.rederived),
        (1, Some(Rederived::Changed))
    );
    assert!(!first.cores.contains(core(3)), "{first:?}");
    assert_eq!(first.profile, checking_offsets(&j));
    j.decide(decided(next));
    let next = same_next(&j);
    let plan = trial_of(&next);
    assert_eq!(plan.load, Load::Many(first.cores));
    assert!(matches!(plan.context, TrialContext::Cycle { .. }));
}

fn r7_cycle_trials(j: &mut Journal) -> HashMap<CoreSet, BTreeSet<(u32, u16)>> {
    let mut seen: HashMap<CoreSet, BTreeSet<(u32, u16)>> = HashMap::new();
    for _ in 0..200 {
        let action = j.next();
        if cycle_end(&action).is_some() {
            j.assert_resumes();
            return seen;
        }
        match action {
            Action::RunTrial(plan) => {
                seen.entry(plan.load.cores())
                    .or_default()
                    .insert((plan.duration.get(), plan.requirement.needed.get()));
                j.trial(&plan, Outcome::Pass);
            }
            other @ (Action::Decide(_) | Action::ReadRanking) => {
                j.decide(decided(other));
            }
        }
    }
    panic!("the cycle did not finish");
}

#[test]
fn part_role_not_core_count() {
    let short_long = |long: u32| BTreeSet::from([(SHORT, 3), (long, 1)]);

    let mut j = journal(one_ccd(), &STARTS, Some(r7_only()));
    let seen = r7_cycle_trials(&mut j);
    assert_eq!(seen[&set(&[0, 1, 2, 3, 4, 5, 6, 7])], short_long(1200));
    assert_eq!(seen[&set(&[1, 2, 3, 4, 5, 6, 7])], short_long(1200));
    assert_eq!(seen[&set(&[2, 3])], short_long(1200));

    let mut j = chain_journal();
    let seen = r7_cycle_trials(&mut j);
    assert_eq!(seen[&set(&[0, 1, 2, 3])], short_long(300));
    assert_eq!(seen[&set(&[1, 2, 3])], short_long(300));
    assert_eq!(seen[&set(&[2, 3])], short_long(300));
    assert_eq!(seen[&set(&[4, 5, 6, 7])], short_long(300));
    assert_eq!(seen[&set(&[0, 1, 2, 3, 4, 5, 6, 7])], short_long(600));

    let mut j = journal(
        grouped(&[&[0, 1], &[2, 3, 4, 5]]),
        &[-20, -20, -10, -20, -30, -40],
        Some(r7_only()),
    );
    let seen = r7_cycle_trials(&mut j);
    assert_eq!(seen[&set(&[0, 1])], short_long(300));
    assert_eq!(seen[&set(&[2, 3, 4, 5])], short_long(300));
    assert_eq!(seen[&set(&[3, 4, 5])], short_long(300));
    assert_eq!(seen[&set(&[4, 5])], short_long(300));
    assert_eq!(seen[&set(&[0, 1, 2, 3, 4, 5])], short_long(600));

    let mut j = journal(
        two_ccds(),
        &STARTS,
        Some(r#"{"checking":{"cycle":["R7"]},"durations":{"checking_all_core_s":480}}"#),
    );
    let seen = r7_cycle_trials(&mut j);
    assert_eq!(seen[&set(&[0, 1, 2, 3])], BTreeSet::from([(SHORT, 4)]));
    assert_eq!(seen[&set(&[1, 2, 3])], BTreeSet::from([(SHORT, 4)]));
    assert_eq!(seen[&set(&[0, 1, 2, 3, 4, 5, 6, 7])], short_long(240));
}

#[test]
fn a_chain_node_replaced_with_empty_truncates_its_descendants() {
    let mut j = chain_journal();
    j.append(chain_record(&j, 1, 1, &[1, 2, 3]), &[]);
    j.append(chain_record(&j, 1, 2, &[2, 3]), &[]);
    j.append(chain_record(&j, 1, 3, &[]), &[]);
    assert!(required(&j, &[1, 2, 3]) && required(&j, &[2, 3]));

    j.append(chain_record(&j, 1, 2, &[]), &[]);
    assert!(required(&j, &[1, 2, 3]));
    assert!(!required(&j, &[2, 3]), "the dropped node is still required");
    j.assert_resumes();

    pass_part(&mut j, &[0, 1, 2, 3], &[]);
    pass_part(&mut j, &[1, 2, 3], &[]);
    pass_part(&mut j, &[4, 5, 6, 7], &[]);
    let next = j.next();
    let chain = chain_of(&next);
    assert_eq!((chain.ccd, chain.part.get()), (CcdId::new(1).unwrap(), 1));

    j.append(chain_record(&j, 1, 1, &[]), &[]);
    assert!(!required(&j, &[1, 2, 3]));
    let next = j.next();
    assert_eq!(chain_of(&next).ccd, CcdId::new(1).unwrap());
    j.assert_resumes();
}

fn assert_ignored(j: &mut Journal, payload: Payload) {
    let before = j.run().clone();
    let next = j.next();
    j.append(payload, &[]);
    let mut after = j.run().clone();
    after.last_seq = before.last_seq;
    assert_eq!(after, before);
    assert_eq!(j.next(), next);
    j.assert_resumes();
}

#[test]
fn chain_events_for_another_cycle_or_a_closed_cycle_are_ignored() {
    let mut j = journal(
        two_ccds(),
        &STARTS,
        Some(r#"{"checking":{"cycle":["R7","R7"]}}"#),
    );
    let mut other = chain_record(&j, 1, 1, &[1, 2, 3]);
    if let Payload::CheckingChain(chain) = &mut other {
        chain.cycle = NonZeroU32::new(2).unwrap();
    }
    assert_ignored(&mut j, other);
    let unstarted = chain_record(&j, 2, 1, &[1, 2, 3]);
    assert_ignored(&mut j, unstarted);
    let mut foreign = chain_record(&j, 1, 1, &[1, 2, 3]);
    if let Payload::CheckingChain(chain) = &mut foreign {
        chain.workload = WorkloadId::new(Regime::R7, 1).unwrap();
    }
    assert_ignored(&mut j, foreign);

    let late = chain_record(&j, 1, 1, &[1, 2, 3]);
    j.cycle_end(1, false, false);
    let checking = j.run().checking.clone();
    j.append(late, &[]);
    assert_eq!(j.run().checking, checking);
    j.assert_resumes();
}

#[test]
fn checking_step_for_a_non_r7_step_is_ignored() {
    let mut j = journal(
        two_ccds(),
        &STARTS,
        Some(r#"{"checking":{"cycle":["R7","R1"]}}"#),
    );
    let r1 = step_record(&j, 2);
    assert_ignored(&mut j, r1);
    let beyond = step_record(&j, 3);
    assert_ignored(&mut j, beyond);
    let mut other = step_record(&j, 1);
    if let Payload::CheckingStep(step) = &mut other {
        step.cycle = NonZeroU32::new(2).unwrap();
    }
    assert_ignored(&mut j, other);
}

#[test]
fn ccd_count_above_three_is_refused_at_session_start() {
    let ids = [0, 1, 2, 3];
    let mut j = Journal::bare(&ids);
    let start = SessionStart {
        build: testkit::build(),
        session: "20231114T221320Z".parse().unwrap(),
        cores: grouped(&[&[0], &[1], &[2], &[3]]),
    };
    assert!(matches!(
        j.try_append(start.into(), &[]),
        Err(FoldError::Topology(TopologyError::TooManyCcds(4)))
    ));
    let three = SessionStart {
        build: testkit::build(),
        session: "20231114T221320Z".parse().unwrap(),
        cores: grouped(&[&[0], &[1], &[2, 3]]),
    };
    assert!(Journal::bare(&ids).try_append(three.into(), &[]).is_ok());
}

/// A cycle open with its first R7 step begun and `cells` pass cells in one of its classes, none
/// at a profile that covers the checking profile, carried before the cycle opened.
fn crowded(cells: u32) -> Journal {
    seeded_journal(two_ccds(), &STARTS, Some(r7_only()), |j| {
        for at in 0..cells {
            let digit = |place: u32| -i64::from(at / 10_u32.pow(place) % 10 + 1);
            j.carry(
                "20230101T000000Z",
                (Regime::R7, 0, &[0, 1, 2, 3], SHORT),
                Condition::Together,
                &[digit(0), digit(1), digit(2), digit(3), -1, -1, -1, -1],
                Outcome::Pass,
            );
        }
    })
}

fn folded_visits(mut j: Journal, event: impl FnOnce(&mut Journal)) -> u64 {
    j.state.visits();
    event(&mut j);
    let folded = j.state.visits();
    j.assert_resumes();
    folded
}

#[test]
fn an_unchanged_config_load_rebuilds_nothing_whatever_the_ledger_holds() {
    let visits = |cells| {
        folded_visits(crowded(cells), |j| {
            j.load(&testkit::config(r7_only()));
        })
    };
    assert_eq!(visits(100), 0);
    assert_eq!(visits(10_000), 0);
}

#[test]
fn a_chain_record_counts_the_classes_it_adds_not_the_ledger() {
    let visits = |cells| {
        let mut j = crowded(cells);
        pass_part(&mut j, &[0, 1, 2, 3], &FULL_REQUESTS);
        let chain = decided(j.next());
        assert!(matches!(chain.payload, DecisionPayload::CheckingChain(_)));
        j.state.visits();
        j.decide(chain);
        let folded = j.state.visits();
        j.assert_resumes();
        folded
    };
    assert_eq!(visits(100), visits(10_000));
}

#[test]
fn a_profile_change_recounts_the_classes_that_can_cover_it_not_the_ledger() {
    let visits = |cells| {
        folded_visits(crowded(cells), |j| {
            j.profile_change(&[-11, -20, -30, -40, -20, -20, -20, -20]);
        })
    };
    assert_eq!(visits(100), visits(10_000));
}

#[test]
fn chains_of_repeated_workload_steps_keep_the_schedule_equal_to_a_rebuild() {
    let repeats = togi_machine::workloads(Regime::R7).len() + 2;
    let cycle = format!(
        r#"{{"checking":{{"cycle":[{}]}}}}"#,
        vec![r#""R7""#; repeats].join(",")
    );
    let mut j = journal(two_ccds(), &STARTS, Some(&cycle));
    let mut chains = 0;
    for _ in 0..2_000 {
        let run_state = j.run();
        let (plan, valid, done) = run_state.debug_rebuild_cycle().unwrap();
        let open = run_state.checking.open().unwrap();
        assert_eq!(open.plan(), plan.as_slice());
        assert_eq!(run_state.checking.counts().unwrap(), &valid);
        assert_eq!(open.steps_done(), done);
        let action = j.next();
        if cycle_end(&action).is_some() {
            assert!(chains > 0, "no chain was recorded");
            return;
        }
        match action {
            Action::Decide(decision) => {
                chains += usize::from(matches!(
                    decision.payload,
                    DecisionPayload::CheckingChain(_)
                ));
                j.decide(decision);
            }
            Action::RunTrial(plan) => {
                run(&mut j, &plan, Outcome::Pass, &FULL_REQUESTS, None);
            }
            other @ Action::ReadRanking => panic!("unexpected action {other:?}"),
        }
    }
    panic!("the cycle never ended");
}

#[test]
fn a_chain_cites_the_passing_together_trial_that_justified_it() {
    let mut j = chain_journal();
    let last = pass_part(&mut j, &[0, 1, 2, 3], &[]);
    let chain = decided(same_next(&j));
    assert!(
        matches!(chain.payload, DecisionPayload::CheckingChain(_)),
        "{chain:?}"
    );
    assert!(chain.cause.contains(&last), "{:?}", chain.cause);
}

#[test]
fn checking_cites_the_last_passing_together_trial_not_an_inconclusive_one() {
    let mut j = chain_journal();
    let plan = j.next_trial();
    let passed = run(&mut j, &plan, Outcome::Pass, &FULL_REQUESTS, None);
    let plan = j.next_trial();
    assert_eq!(plan.cause, vec![passed]);
    run(&mut j, &plan, Outcome::Inconclusive, &[], None);
    assert_eq!(j.next_trial().cause, vec![passed]);
    let start = j.run().checking.open().unwrap().start;
    let mut last = passed;
    for _ in 0..2_000 {
        let action = same_next(&j);
        if cycle_end(&action).is_some() {
            assert_eq!(decided(action).cause, vec![start, last]);
            return;
        }
        last = match action {
            Action::Decide(decision) => j.decide(decision),
            Action::RunTrial(plan) => {
                assert_eq!(plan.cause, vec![last], "{plan:?}");
                run(&mut j, &plan, Outcome::Pass, &FULL_REQUESTS, None)
            }
            other @ Action::ReadRanking => panic!("unexpected action {other:?}"),
        };
    }
    panic!("the cycle never ended");
}

#[test]
fn only_a_passing_together_end_advances_the_checking_last_seq() {
    let mut j = chain_journal();
    let before = j.run().checking.last_seq();
    let plan = j.next_trial();
    run(&mut j, &plan, Outcome::Inconclusive, &[], None);
    assert_eq!(j.run().checking.last_seq(), before);
    let plan = j.next_trial();
    run(&mut j, &plan, Outcome::Failure, &[], None);
    assert_eq!(j.run().checking.last_seq(), before);
}

#[test]
fn a_passing_alone_end_leaves_the_checking_last_seq() {
    let mut alone = Journal::search_at(&[-10]);
    let before = alone.run().checking.last_seq();
    let search = alone.next_trial();
    assert_eq!(search.condition, Condition::Alone, "{search:?}");
    run(&mut alone, &search, Outcome::Pass, &[], None);
    assert_eq!(alone.run().checking.last_seq(), before);
}

#[test]
fn a_passing_parked_end_leaves_the_checking_last_seq() {
    let mut zero = Journal::has_room(&[0, -10]);
    let failing = zero.manual(
        Regime::R5,
        0,
        Load::Single(core(0)),
        Condition::Together,
        &[0, -10],
        SHORT,
    );
    run(&mut zero, &failing, Outcome::Failure, &[], Some(0));
    let attribution = zero.next_decision();
    zero.decide(attribution);
    let before = zero.run().checking.last_seq();
    let rerun = zero.next_trial();
    assert_eq!(rerun.condition, Condition::Parked, "{rerun:?}");
    assert!(
        matches!(rerun.context, TrialContext::ZeroRerun { .. }),
        "{rerun:?}"
    );
    run(&mut zero, &rerun, Outcome::Pass, &[], None);
    assert_eq!(zero.run().checking.last_seq(), before);
}
