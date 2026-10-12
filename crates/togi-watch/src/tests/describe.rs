use togi_journal::HuntOutcome::{Cancelled, Combination, Culprit, Fallback, Loaded};
use togi_journal::{
    CheckingStep, CorePhase, CycleEvent, DeadEnd, DeadEndCondition, DeepeningRound, HuntOutcome,
    HuntStage, Inference, Mce, Member, Outcome, Payload, RoundEvent, SessionCarried, Shutdown,
    ShutdownReason, TrialId,
};
use togi_machine::{BankType, Condition, CpuId, Regime, Signal};

use super::kit::*;

fn phase(id: u8, from: CorePhase, to: CorePhase, offset: i64) -> Payload {
    let mut start = Start::search(offset);
    (start.from, start.to, start.pass) = (Some(from), to, Some(offset));
    transition(id, start).into()
}

fn member(core: u8, offset: i64) -> Member {
    Member {
        core: self::core(core),
        offset: off(offset),
    }
}

fn last(h: &Hist) -> String {
    h.sentences().pop().unwrap()
}

#[test]
fn a_known_failure_names_the_hunt_it_leads_to() {
    let mut h = Hist::three();
    let carried = h.push(carried_trial().into());
    let mut known = failure(Signal::ComputationError, None);
    known.regime = Some(Regime::R7);
    known.known_failure = Some(carried);
    h.push(known.into());
    assert_eq!(
        last(&h),
        "skip: R7 all-core at these offsets already failed in a carried trial"
    );
    h.push(hunt_start(1, &[0, 2]).into());
    assert_eq!(
        last(&h),
        "skip: R7 all-core on 00-02 at these offsets already failed in a carried trial → hunt 1"
    );
}

#[test]
fn a_known_crash_without_a_regime() {
    let mut h = Hist::three();
    let earlier = h.push(warning("an earlier event"));
    let mut known = failure(Signal::Crash, None);
    known.known_failure = Some(earlier);
    h.push(known.into());
    assert_eq!(last(&h), "skip: a trial at these offsets already crashed");
}

#[test]
fn solo_limits_found_before_the_first_trial_are_counted_on_the_session_line() {
    let mut h = Hist::three();
    h.push(
        SessionCarried {
            sources: Vec::new(),
            failure_points: false,
            detail: None,
            carried: Vec::new(),
        }
        .into(),
    );
    h.push(phase(0, CorePhase::Search, CorePhase::HasRoom, -20));
    h.push(phase(1, CorePhase::Search, CorePhase::AtLimit, -25));
    let mut bare = Start::search(-9);
    (bare.from, bare.to) = (Some(CorePhase::Search), CorePhase::AtLimit);
    h.push(transition(2, bare).into());
    assert_eq!(
        h.sentences(),
        ["start: session started · 2 solo limits carried from the last session"]
    );
    let plan = intent(
        "t1",
        Regime::R7,
        Condition::Together,
        &[-20, -25, 0],
        &[0, 1, 2],
    );
    h.push(plan.into());
    h.push(phase(2, CorePhase::Search, CorePhase::HasRoom, -10));
    assert_eq!(
        h.sentences().len(),
        2,
        "a solo limit found in a trial is its own line"
    );
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
fn a_backoff_names_the_combination_it_clears() {
    let mut h = Hist::three();
    h.push(hunt_start(1, &[0, 2]).into());
    let id = h.push(combination(1, 1, &[(0, -30), (2, -30)], false).into());
    let mut decision = super::words::decision(togi_journal::Decision::Backoff, -30, -16);
    decision.core = core(0);
    h.push_with(decision.into(), &[id]);
    assert_eq!(last(&h), "backoff: core 00 -30 → -16 · clear of C1");
}

#[test]
fn a_checking_step_names_its_regime_then_its_workload() {
    let mut h = Hist::three();
    let mut start = cycle(1, CycleEvent::Start);
    start.steps = vec![Regime::R1, Regime::R2];
    h.push(start.into());
    let step = CheckingStep {
        cycle: nz(1),
        step: nz(2),
        profile: offsets(&[0, 0, 0]),
    };
    h.push(step.clone().into());
    assert_eq!(last(&h), "step: cycle 1 step 2: R2 heavy vector");
    let mut plan = intent(
        "s2",
        Regime::R2,
        Condition::Together,
        &[0, 0, 0],
        &[0, 1, 2],
    );
    plan.cycle = Some(nz(1));
    plan.step = Some(nz(2));
    h.push(plan.clone().into());
    let named = last(&h);
    assert!(
        named.starts_with("step: cycle 1 step 2: R2 heavy vector, "),
        "{named}"
    );
    h.push(plan.into());
    assert_eq!(last(&h), named, "the workload is named once");
    let mut unknown = step;
    unknown.cycle = nz(9);
    unknown.step = nz(1);
    h.push(unknown.into());
    assert_eq!(last(&h), "step: cycle 9 step 1");
}

#[test]
fn hunt_groups_answered_without_a_trial() {
    let mut h = Hist::three();
    h.push(hunt_start(1, &[0, 2]).into());
    let mut skipped = hunt_group(1, 3, &[0]);
    skipped.skipped = true;
    skipped.reason = Some("same set".to_owned());
    h.push(skipped.into());
    assert_eq!(last(&h), "skip: hunt 1 group 3 skipped: same set");
    let mut probe = hunt_group(1, 4, &[0]);
    probe.probe = Some(member(2, -19));
    probe.inferred = Some(Inference::Pass);
    h.push(probe.clone().into());
    assert_eq!(
        last(&h),
        "probe: hunt 1 group 4 · core 02 at -19 · passed on existing evidence"
    );
    probe.group = nz(5);
    probe.inferred = Some(Inference::Failure);
    h.push(probe.into());
    assert!(last(&h).ends_with("failed on existing evidence"));
}

#[test]
fn a_locating_hunt_names_its_first_group() {
    let mut h = Hist::three();
    h.push(hunt_start(1, &[0, 2]).into());
    assert_eq!(last(&h), "hunt: #1 started · candidates 00 02");
    let mut locate = hunt_group(1, 1, &[]);
    locate.stage = HuntStage::Locate;
    h.push(locate.into());
    assert_eq!(last(&h), "hunt: #1 started · locate: 00 02 at 0");
}

#[test]
fn a_hunt_names_its_first_part() {
    let mut h = Hist::three();
    h.push(hunt_start(1, &[0, 1, 2]).into());
    h.push(hunt_group(1, 1, &[0, 1]).into());
    assert_eq!(
        last(&h),
        "hunt: #1 started · part 1: 00 01 at failing offsets, 02 parked"
    );
    h.push(hunt_group(1, 2, &[2]).into());
    assert!(
        last(&h).contains("part 1"),
        "only the first group renames the hunt"
    );
}

fn ended_hunt(
    h: &mut Hist,
    hunt: u32,
    groups: u32,
    live: &[u32],
    inferred: &[u32],
    result: HuntOutcome,
    cores: &[u8],
) -> String {
    h.push(hunt_start(hunt, &[0, 2]).into());
    for &group in live {
        h.push(hunt_group(hunt, group, &[0]).into());
    }
    for &group in inferred {
        let mut answered = hunt_group(hunt, group, &[0]);
        answered.inferred = Some(Inference::Pass);
        h.push(answered.into());
    }
    let mut end = hunt_end(hunt, result, cores);
    end.groups = groups;
    h.push(end.into());
    last(h)
}

#[test]
fn a_hunt_says_how_it_ended() {
    let mut h = Hist::unbounded_three();
    assert_eq!(
        ended_hunt(&mut h, 1, 2, &[1, 2], &[], Culprit, &[2]),
        "hunt: #1 done · 2 groups in 3s · core 02 named"
    );
    assert_eq!(
        ended_hunt(&mut h, 2, 1, &[], &[1], Culprit, &[0, 2]),
        "hunt: #2 done · 1 group, all answered by existing evidence · cores 00 02 named"
    );
    assert_eq!(
        ended_hunt(&mut h, 3, 0, &[], &[], Combination, &[0, 2]),
        "hunt: #3 done · 00 02 kept together"
    );
    assert_eq!(
        ended_hunt(&mut h, 4, 0, &[], &[], Fallback, &[0, 2]),
        "hunt: #4 done · 00 02 unresolved"
    );
    assert_eq!(
        ended_hunt(&mut h, 5, 0, &[], &[], Loaded, &[0, 2]),
        "hunt: #5 done · stays with loaded 00 02"
    );
    assert_eq!(
        ended_hunt(&mut h, 6, 0, &[], &[], Cancelled, &[0, 2]),
        "hunt: #6 cancelled"
    );
}

#[test]
fn a_probed_combination_says_so() {
    let mut h = Hist::unbounded_three();
    h.push(hunt_start(1, &[0, 2]).into());
    let mut probe = hunt_group(1, 1, &[0]);
    probe.probe = Some(member(2, -19));
    h.push(probe.into());
    h.push(hunt_end(1, HuntOutcome::Combination, &[0, 2]).into());
    assert_eq!(last(&h), "hunt: #1 done · 00 02 kept together, probed");
}

#[test]
fn deepening_rounds_say_their_moves_and_their_verdict() {
    let round = |event, passed, reason: Option<&str>| DeepeningRound {
        round: nz(3),
        event,
        base: offsets(&[]),
        base_seq: None,
        target: offsets(&[]),
        profile: offsets(&[-12, -40]),
        cores: set(&[0, 1]),
        ranking: Vec::new(),
        trials: None,
        trial_s: None,
        passed,
        reason: reason.map(str::to_owned),
    };
    let mut h = Hist::three();
    h.push(round(RoundEvent::Start, false, None).into());
    assert_eq!(
        last(&h),
        "round: #3: core 00 goes deeper to -12, core 01 goes deeper to -40"
    );
    h.push(round(RoundEvent::End, true, None).into());
    assert_eq!(last(&h), "round: #3 passed, the proposed offsets held");
    h.push(round(RoundEvent::End, false, Some("a failure")).into());
    assert_eq!(last(&h), "round: #3 stopped: a failure");
}

struct Trial {
    h: Hist,
    n: u32,
}

impl Trial {
    fn new() -> Self {
        Self {
            h: Hist::three(),
            n: 0,
        }
    }

    fn run(
        &mut self,
        shape: impl FnOnce(&mut togi_journal::TrialIntent),
        end: impl FnOnce(&mut togi_journal::TrialEnd),
    ) -> Option<String> {
        self.n += 1;
        let id = format!("t{}", self.n);
        let mut plan = intent(
            &id,
            Regime::R7,
            Condition::Together,
            &[-20, -30, -10],
            &[0, 1, 2],
        );
        shape(&mut plan);
        self.h.push(plan.into());
        let mut over = end_payload(
            &TrialId::from(id),
            Outcome::Failure,
            Some(Signal::ComputationError),
        );
        end(&mut over);
        let before = self.h.sentences().len();
        self.h.push(over.into());
        let after = self.h.sentences();
        (after.len() > before).then(|| after.last().unwrap().clone())
    }
}

#[test]
fn a_trial_is_named_by_what_it_ran() {
    let mut t = Trial::new();
    assert_eq!(
        t.run(
            |i| {
                i.regime = Regime::R1;
                i.condition = Condition::Alone;
                i.core = Some(core(0));
                i.cores = set(&[0]);
                i.offset = Some(off(-20));
            },
            |_| {}
        )
        .as_deref(),
        Some("fail: core 00 · R1 light at -20 · wrong result")
    );
    assert_eq!(
        t.run(
            |i| {
                i.regime = Regime::R1;
                i.condition = Condition::Alone;
                i.cores = set(&[0]);
            },
            |_| {}
        )
        .as_deref(),
        Some("fail: R1 light on core 00 at -20 · wrong result"),
        "an alone trial that recorded no offset is named by its profile"
    );
    assert_eq!(
        t.run(
            |i| {
                i.condition = Condition::Alone;
                i.core = Some(core(0));
                i.cores = set(&[0]);
            },
            |e| e.outcome = Outcome::Pass
        ),
        None,
        "an alone pass says nothing"
    );
    assert_eq!(
        t.run(|_| {}, |e| e.core = Some(core(1))).as_deref(),
        Some("fail: R7 all-core on 00-02 · wrong result · core 01 named")
    );
    assert_eq!(
        t.run(|_| {}, |_| {}).as_deref(),
        Some("fail: R7 all-core on 00-02 · wrong result · no core named")
    );
    assert_eq!(
        t.run(|i| i.rerun = true, |_| {}).as_deref(),
        Some("fail: rerun of R7 all-core on 00-02 · wrong result · no core named")
    );
    assert_eq!(
        t.run(
            |i| {
                i.rerun = true;
                i.condition = Condition::Parked;
            },
            |_| {}
        )
        .as_deref(),
        Some(
            "fail: rerun of R7 all-core on 00-02 with every core at 0 · wrong result · no core named"
        )
    );
    assert_eq!(
        t.run(
            |i| i.regime = Regime::R1,
            |e| e.outcome = Outcome::Inconclusive
        )
        .as_deref(),
        Some("unclear: R1 light on 00-02 · it could not run")
    );
    assert_eq!(
        t.run(
            |i| i.regime = Regime::R1,
            |e| {
                e.outcome = Outcome::Inconclusive;
                e.reason = Some("no backend".to_owned());
            }
        )
        .as_deref(),
        Some("unclear: R1 light on 00-02 · no backend")
    );
}

#[test]
fn an_r7_cycle_trial_names_the_part_it_loaded() {
    let mut t = Trial::new();
    let part = |ids: &'static [u8]| {
        move |i: &mut togi_journal::TrialIntent| {
            i.cycle = Some(nz(1));
            i.cores = set(ids);
        }
    };
    assert_eq!(
        t.run(part(&[0]), |e| e.core = Some(core(0))).as_deref(),
        Some("fail: R7 partial CCD 0 on core 00 at -20 · wrong result · core 00 named")
    );
    assert_eq!(
        t.run(part(&[0, 1]), |e| e.core = Some(core(0))).as_deref(),
        Some("fail: R7 full CCD 0 on 00 01 · wrong result · core 00 named")
    );
    assert_eq!(
        t.run(part(&[0, 1, 2]), |e| e.core = Some(core(0)))
            .as_deref(),
        Some("fail: R7 full CCD 0+1 on 00-02 · wrong result · core 00 named")
    );
}

#[test]
fn a_crash_says_how_late_it_came() {
    let mut t = Trial::new();
    let crash_at = |sample: Option<u32>| {
        move |e: &mut togi_journal::TrialEnd| {
            e.signal = Some(Signal::Crash);
            e.last_sample_s = sample;
        }
    };
    assert_eq!(
        t.run(|_| {}, crash_at(Some(100))).as_deref(),
        Some("crash: R7 all-core on 00-02 · rebooted · late in the trial · no core named")
    );
    assert_eq!(
        t.run(|_| {}, crash_at(None)).as_deref(),
        Some("crash: R7 all-core on 00-02 · rebooted · no core named")
    );
    assert_eq!(
        t.run(|i| i.duration_s = 0, crash_at(Some(5))).as_deref(),
        Some("crash: R7 all-core on 00-02 · rebooted · no core named")
    );
}

#[test]
fn a_hunt_trial_is_named_by_its_group() {
    let mut t = Trial::new();
    let hunting = |group: u32| {
        move |i: &mut togi_journal::TrialIntent| {
            i.hunt = Some(nz(1));
            i.group = Some(nz(group));
        }
    };
    assert_eq!(
        t.run(hunting(1), |_| {}).as_deref(),
        Some("fail: hunt 1 group 1 · wrong result")
    );
    t.h.push(hunt_start(1, &[0, 1]).into());
    t.h.push(hunt_group(1, 2, &[0, 1]).into());
    assert_eq!(
        t.run(hunting(2), |_| {}).as_deref(),
        Some("fail: hunt 1 group 2 · 00 01 at failing offsets · wrong result")
    );
    let mut locate = hunt_group(1, 3, &[]);
    locate.stage = HuntStage::Locate;
    t.h.push(locate.into());
    assert_eq!(
        t.run(hunting(3), |_| {}).as_deref(),
        Some("fail: hunt 1 group 3 · locate, idle cores at 0 · wrong result")
    );
    let mut probe = hunt_group(1, 4, &[0]);
    probe.probe = Some(member(2, -19));
    probe.held = vec![member(0, -20), member(1, -20)];
    t.h.push(probe.into());
    assert_eq!(
        t.run(hunting(4), |_| {}).as_deref(),
        Some("fail: hunt 1 group 4 · 00-02 with core 02 at -19 · wrong result")
    );
}

#[test]
fn a_failed_hunt_group_keeps_its_signal() {
    let mut t = Trial::new();
    t.run(
        |i| {
            i.hunt = Some(nz(1));
            i.group = Some(nz(2));
        },
        |e| e.signal = Some(Signal::Stall),
    );
    assert_eq!(
        t.h.facts.group_signals.get(&(1, 2)).copied(),
        Some(Signal::Stall)
    );
}

#[test]
fn the_kinds_with_a_line_of_their_own() {
    let mut h = Hist::three();
    h.push(
        Mce {
            cpu: CpuId(3),
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
        "mce: cpu 3 corrected error between trials (recorded only)"
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
fn a_hunt_group_with_no_hunt_start_keeps_no_count() {
    let mut h = Hist::three();
    h.push(hunt_group(5, 1, &[0]).into());
    assert_eq!(h.sentences().len(), 1);
    assert!(h.facts.hunt_starts.is_empty());
}

#[test]
fn a_hunt_line_the_history_has_dropped_is_not_renamed() {
    let mut h = Hist::new(&[(0, 0), (1, 0), (2, 1)]);
    h.facts.history.limit = 2;
    h.push(hunt_start(1, &[0, 2]).into());
    for n in 0..8 {
        h.push(warning(&format!("warning {n}")));
    }
    h.push(hunt_group(1, 1, &[0]).into());
    assert!(
        h.sentences()
            .iter()
            .all(|line| !line.contains("#1 started"))
    );
    assert_eq!(h.facts.hunt_starts[&1].live, 1);
}
