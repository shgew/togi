//! The stage line and Main's phase-progress rows, and the [`Rig`] the NOW rows' tests share.

use std::num::{NonZeroU16, NonZeroU32};
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::{
    Activity, DeadEndCondition, HuntStage, Outcome, ProfileChange, ShutdownReason, TrialId,
};
use togi_machine::{
    CcdId, Condition, CoreSet, ObservedOffset, PerCore, Regime, WorkloadId, workloads,
};
use togi_tuner::{
    Action, Between, Candidate, ClassKey, CyclePart, CyclePlan, CycleStep, DeepeningCheck,
    DeepeningView, Forecast, GroupState, HuntPlan, Load, PhaseNo, PhasePlan, Profile, Requirement,
    RequirementKind, RerunPlan, Secs, StepNo, Topology, TrialContext, TrialPlan, Window,
    WorkloadChoice,
};

use crate::frames::colours::Paint;
use crate::frames::fit::plain;
use crate::frames::scene::Scene;
use crate::model::{
    CoreEvidence, CoreState, CoreView, Count, CycleView, DeadEndView, HuntCause, HuntFacts,
    HuntView, Model, RecoveryView, SessionView, Snapshot, StopView, TrialEndView, TrialView,
};
use crate::screen::Colours;
use crate::tests::kit::{Script, Start, core, infos, nz, off, seq, set};

use super::{phase_progress, stage_line};

pub(in crate::frames::dash) fn at(text: &str) -> Timestamp {
    text.parse().expect("a timestamp")
}

/// Everything one frame of a session borrows, owned in one place so a test names only what it varies.
pub(in crate::frames::dash) struct Rig {
    pub(in crate::frames::dash) model: Model,
    pub(in crate::frames::dash) session: SessionView,
    pub(in crate::frames::dash) cores: Vec<CoreView>,
    pub(in crate::frames::dash) phases: Option<PhasePlan>,
    pub(in crate::frames::dash) deepening: Option<DeepeningView>,
    pub(in crate::frames::dash) plan: Option<CyclePlan>,
    pub(in crate::frames::dash) resume: Option<StepNo>,
    pub(in crate::frames::dash) hunt: Option<(HuntPlan, HuntFacts)>,
    pub(in crate::frames::dash) trial: Option<TrialView>,
    pub(in crate::frames::dash) stopped: Option<StopView>,
    pub(in crate::frames::dash) dead_end: Option<DeadEndView>,
    pub(in crate::frames::dash) recover: Option<RecoveryView>,
    pub(in crate::frames::dash) clean_cycles: u32,
    pub(in crate::frames::dash) forecast: Forecast,
    pub(in crate::frames::dash) last: Option<TrialEndView>,
    pub(in crate::frames::dash) now: Timestamp,
    zone: TimeZone,
}

impl Rig {
    /// A session over `total` cores of which the first `found` have a solo limit.
    pub(in crate::frames::dash) fn new(total: u8, found: u8) -> Self {
        let start = at("2026-10-12T10:00:00Z");
        Self {
            model: Model::new(),
            session: SessionView {
                start,
                activity: None,
            },
            cores: (0..total).map(|id| core_view(id, id < found)).collect(),
            phases: None,
            deepening: None,
            plan: None,
            resume: None,
            hunt: None,
            trial: None,
            stopped: None,
            dead_end: None,
            recover: None,
            clean_cycles: 0,
            forecast: Forecast::default(),
            last: None,
            now: start,
            zone: TimeZone::UTC,
        }
    }

    /// Phase `phase` with every solo limit found.
    pub(in crate::frames::dash) fn in_phase(mut self, phase: PhaseNo) -> Self {
        for core in &mut self.cores {
            core.solo = Some(off(-10));
        }
        self.phases = Some(PhasePlan {
            phase,
            phase1_end: None,
            concluded: None,
            confirming: false,
            rounds_left: 2,
            round: None,
            candidates: Vec::new(),
        });
        self
    }

    pub(in crate::frames::dash) fn snapshot(&self) -> Snapshot<'_> {
        let mut s = self.model.snapshot();
        s.session = Some(&self.session);
        s.cores = &self.cores;
        s.phases = self.phases.as_ref();
        s.deepening = self.deepening.as_ref();
        s.cycle = self.plan.as_ref().map(|plan| CycleView {
            plan,
            step_hunts: &[],
            resume: self.resume,
        });
        s.hunt = self
            .hunt
            .as_ref()
            .map(|(plan, facts)| HuntView { plan, facts });
        s.trial = self.trial.as_ref();
        s.stopped = self.stopped.as_ref();
        s.dead_end = self.dead_end.as_ref();
        s.recover = self.recover.as_ref();
        s.clean_cycles = self.clean_cycles;
        s.forecast = &self.forecast;
        s.last = self.last.as_ref();
        s
    }

    pub(in crate::frames::dash) fn scene(&self) -> Scene<'_> {
        Scene::new(self.snapshot(), self.now, &self.zone, Colours::Full)
    }

    fn stage(&self, width: usize) -> String {
        plain(&stage_line(&self.scene(), width, false))
    }

    /// The stage line at one cell short of its full form.
    fn cramped(&self) -> String {
        self.stage(self.stage(WIDE).chars().count() - 1)
    }

    fn progress(&self) -> [String; 2] {
        phase_progress(&self.scene(), 200).map(|line| plain(&line))
    }
}

const WIDE: usize = 240;

fn core_view(id: u8, found: bool) -> CoreView {
    CoreView {
        id: core(id),
        ccd: CcdId(0),
        state: CoreState::Search,
        queued: None,
        applied: Some(ObservedOffset(0)),
        profile: off(0),
        solo: found.then(|| off(-10)),
        pass: None,
        next: None,
        fail: None,
        holder: None,
        gave_back: None,
        group_fails: Vec::new(),
        returns_to: None,
        confirm: None,
        loaded: false,
        judged: false,
        evidence: CoreEvidence::default(),
    }
}

pub(in crate::frames::dash) fn trial(
    regime: Regime,
    started: Option<Timestamp>,
    secs: u64,
) -> TrialView {
    TrialView {
        id: TrialId::from("t1".to_owned()),
        regime,
        workload: WorkloadId::new(regime, 0).expect("a workload"),
        cores: CoreSet::default(),
        parked: CoreSet::default(),
        idle: CoreSet::default(),
        condition: Condition::Together,
        activity: None,
        profile: PerCore::default(),
        started,
        crashed: false,
        duration: Duration::from_secs(secs),
        partial: false,
        rerun: false,
        retry: false,
        cycle: None,
        hunt: None,
        core: None,
        offset: None,
        round: None,
        count: Count::default(),
    }
}

fn part(short: u32, long: u32, passed: u32, running: bool) -> CyclePart {
    CyclePart {
        cores: set(&[0]),
        ccd: Some(CcdId(0)),
        full: true,
        partial: false,
        short,
        short_s: 60,
        long,
        long_s: 600,
        passed,
        failed: 0,
        done: passed >= short + long,
        running,
    }
}

/// A part whose count runs past what it requires and still reads as unfinished.
fn overshooting_part() -> CyclePart {
    CyclePart {
        done: false,
        ..part(3, 1, 9, true)
    }
}

fn step(regime: Regime, parts: Vec<CyclePart>, chains_complete: bool) -> CycleStep {
    CycleStep {
        regime,
        workload: workloads(regime)[0].id(),
        done: parts.iter().all(|part| part.done) && !parts.is_empty(),
        parts,
        chains_complete,
    }
}

/// Cycle 2 with `steps`, the tuner's current step `current` (counting from 0).
fn cycle(steps: Vec<CycleStep>, current: usize, paused: bool) -> CyclePlan {
    CyclePlan {
        number: NonZeroU32::new(2),
        open: true,
        steps,
        current,
        paused,
    }
}

/// Three steps: the first done, the second wanting a long check after three short ones.
fn checking_plan(paused: bool) -> CyclePlan {
    cycle(
        vec![
            step(Regime::R1, vec![part(1, 0, 1, false)], true),
            step(Regime::R3, vec![part(3, 1, 3, true)], true),
            step(Regime::R4, vec![part(1, 0, 0, false)], true),
        ],
        1,
        paused,
    )
}

fn hunt(number: u32, cut: u32) -> (HuntPlan, HuntFacts) {
    let plan = HuntPlan {
        number,
        regime: Regime::R7,
        failure: seq(1),
        trial: None,
        candidates: set(&[0, 1]),
        cut,
        parts: Vec::new(),
        groups: Vec::new(),
        probes: Vec::new(),
        rerun: RerunPlan {
            regime: Regime::R7,
            cores: CoreSet::default(),
            short: 0,
            short_s: 0,
            long: 0,
            long_s: 0,
        },
    };
    let facts = HuntFacts {
        started: None,
        cause: HuntCause {
            at: None,
            trial: None,
            end: None,
            signal: None,
            core: None,
            known: false,
            carried: false,
            regime: None,
            rerun_of: false,
            trial_number: 0,
        },
        parking: None,
        group_signals: Vec::new(),
    };
    (plan, facts)
}

fn profile() -> Profile {
    let topology = Topology::new(&infos(&[(0, 0), (1, 0), (2, 0)])).expect("a topology");
    let offsets = [0u8, 1, 2].iter().map(|&id| (core(id), off(-10))).collect();
    Profile::new(&topology, offsets).expect("a profile")
}

fn round(checks: Vec<DeepeningCheck>) -> DeepeningView {
    DeepeningView {
        round: nz(3),
        seq: seq(5),
        target: profile(),
        profile: profile(),
        cores: set(&[0, 1]),
        checks,
    }
}

fn check(passed: u32, needed: u32, running: bool) -> DeepeningCheck {
    DeepeningCheck {
        regime: Regime::R1,
        workload: workloads(Regime::R1)[0].id(),
        cores: set(&[0]),
        condition: Condition::Alone,
        profile: profile(),
        needed,
        passed,
        failed: 0,
        running,
    }
}

fn candidates() -> Vec<Candidate> {
    [(0, true), (1, true), (2, false)]
        .into_iter()
        .map(|(id, moving)| Candidate {
            core: core(id),
            offset: off(-12),
            solo_limit: Some(off(-10)),
            gap: 2,
            carried: false,
            moving,
        })
        .collect()
}

fn phase1() -> Rig {
    let mut rig = Rig::new(3, 3).in_phase(PhaseNo::One);
    rig.plan = Some(checking_plan(false));
    rig
}

fn phase2_round() -> Rig {
    let mut rig = Rig::new(3, 3).in_phase(PhaseNo::Two);
    if let Some(phases) = rig.phases.as_mut() {
        phases.round = Some(3);
        phases.candidates = candidates();
    }
    rig.deepening = Some(round(vec![
        check(3, 3, false),
        check(1, 3, true),
        check(0, 2, false),
    ]));
    rig
}

fn confirmation() -> Rig {
    let mut rig = Rig::new(3, 3).in_phase(PhaseNo::Two);
    if let Some(phases) = rig.phases.as_mut() {
        phases.confirming = true;
    }
    rig.plan = Some(checking_plan(false));
    rig
}

fn clean() -> Rig {
    let mut rig = Rig::new(3, 3).in_phase(PhaseNo::Concluded);
    rig.plan = Some(checking_plan(false));
    rig.clean_cycles = 4;
    rig
}

#[test]
fn the_stage_line_names_the_pipeline_and_marks_the_search() {
    let mut rig = Rig::new(3, 1);
    assert_eq!(
        rig.stage(WIDE),
        "► SOLO LIMITS  1/3 → ○ PHASE 1 → ○ PHASE 2 → ○ CLEAN CYCLES"
    );
    assert_eq!(rig.cramped(), "► SOLO  1/3 → ○ P1 → ○ P2 → ○ CLEAN");
    rig.cores[0].solo = None;
    assert!(rig.stage(WIDE).starts_with("► SOLO LIMITS  0/3"));
}

#[test]
fn the_stage_line_takes_its_form_from_the_width_it_has() {
    let rig = Rig::new(3, 1);
    let full = rig.stage(WIDE);
    let cells = full.chars().count();
    assert_eq!(rig.stage(cells), full, "the full names fit exactly");
    assert_eq!(rig.stage(cells - 1), rig.cramped());
    assert!(rig.cramped().contains("P1"));
}

#[test]
fn phase_one_is_current_once_every_solo_limit_is_found() {
    assert_eq!(
        phase1().stage(WIDE),
        "■ SOLO LIMITS  3/3 → ► PHASE 1 · cycle 2 → ○ PHASE 2 → ○ CLEAN CYCLES"
    );
}

#[test]
fn a_hunt_is_an_amber_sub_state_of_the_phase_it_interrupts() {
    let mut rig = phase1();
    rig.hunt = Some(hunt(14, 3));
    let line = stage_line(&rig.scene(), WIDE, false);
    let text = plain(&line);
    assert!(
        text.contains("► PHASE 1 · cycle 2 · HUNT 14 · starting"),
        "{text}"
    );
    assert!(!text.contains("○ PHASE 1"), "{text}");
    let hunt_span = line
        .spans
        .iter()
        .find(|span| span.content.contains("HUNT 14"))
        .expect("the hunt span");
    assert_eq!(hunt_span.style, Paint::Hunt.style(Colours::Full));
}

#[test]
fn a_repair_holds_the_cycle_without_leaving_the_phase() {
    let mut rig = phase1();
    rig.plan = Some(checking_plan(true));
    assert!(
        rig.stage(WIDE).contains("► PHASE 1 · cycle 2 · repairing"),
        "{}",
        rig.stage(WIDE)
    );
    rig.trial = Some(TrialView {
        rerun: true,
        ..trial(Regime::R3, None, 60)
    });
    rig.plan = Some(checking_plan(false));
    assert!(rig.stage(WIDE).contains("· repairing"));
}

#[test]
fn phase_two_names_its_round_and_its_confirmation_cycle() {
    assert_eq!(
        phase2_round().stage(WIDE),
        "■ SOLO LIMITS  3/3 → ■ PHASE 1 → ► PHASE 2 · round 3 · 2 rounds left if none fails → ○ CLEAN CYCLES"
    );
    assert_eq!(
        confirmation().stage(WIDE),
        "■ SOLO LIMITS  3/3 → ■ PHASE 1 → ► PHASE 2 · confirmation cycle → ○ CLEAN CYCLES"
    );
    assert!(
        confirmation().cramped().contains("► P2 · confirm"),
        "{}",
        confirmation().cramped()
    );
}

#[test]
fn clean_cycles_are_current_after_phase_two_and_repeat() {
    assert_eq!(
        clean().stage(WIDE),
        "■ SOLO LIMITS  3/3 → ■ PHASE 1 → ■ PHASE 2 → ► CLEAN CYCLES · repeats until stopped"
    );
}

#[test]
fn a_stop_keeps_the_finished_stages_and_marks_nothing_running() {
    let mut rig = phase2_round();
    rig.stopped = Some(StopView {
        at: rig.now,
        reason: ShutdownReason::Signal,
        saved: false,
    });
    let text = rig.stage(WIDE);
    assert_eq!(
        text,
        "STOPPED  ■ SOLO LIMITS  3/3 → ■ PHASE 1 → ○ PHASE 2 → ○ CLEAN CYCLES"
    );
    assert!(!text.contains('►'));
    let mut clean = clean();
    clean.stopped = rig.stopped;
    let text = clean.stage(WIDE);
    assert!(text.starts_with("STOPPED  ■ SOLO LIMITS"), "{text}");
    assert!(text.ends_with("○ CLEAN CYCLES"), "{text}");
}

#[test]
fn recovery_and_a_dead_end_lead_the_line_and_suppress_the_running_marker() {
    let mut rig = phase1();
    rig.recover = Some(RecoveryView {
        crash_at: rig.now,
        reset: None,
        trial: None,
        end: None,
    });
    let text = rig.stage(WIDE);
    assert!(text.starts_with("RECOVERY  ■ SOLO LIMITS"), "{text}");
    assert!(!text.contains('►'), "{text}");
    rig.recover = None;
    let mut crashed = trial(Regime::R1, Some(rig.now), 60);
    crashed.crashed = true;
    rig.trial = Some(crashed);
    assert!(rig.stage(WIDE).starts_with("RECOVERY"));
    rig.trial = None;
    rig.dead_end = Some(DeadEndView {
        at: rig.now,
        condition: DeadEndCondition::BootLoop,
        detail: String::new(),
    });
    let text = rig.stage(WIDE);
    assert!(text.starts_with("DEAD END  ■ SOLO LIMITS"), "{text}");
    assert!(!text.contains('►'), "{text}");
}

#[test]
fn a_session_less_scene_has_no_stage_line_and_no_progress() {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let scene = Scene::new(
        model.snapshot(),
        at("2026-10-12T10:00:00Z"),
        &zone,
        Colours::None,
    );
    assert!(plain(&stage_line(&scene, WIDE, false)).is_empty());
    assert!(
        phase_progress(&scene, 80)
            .iter()
            .all(|line| plain(line).is_empty())
    );
}

#[test]
fn a_summary_stage_line_ends_in_the_trials_time() {
    let mut rig = phase1();
    let started = rig.now;
    rig.trial = Some(trial(Regime::R3, Some(started), 600));
    rig.now = at("2026-10-12T10:04:00Z");
    let summary = |rig: &Rig| plain(&stage_line(&rig.scene(), WIDE, true));
    assert!(summary(&rig).ends_with("6:00 left"));
    rig.now = at("2026-10-12T10:11:00Z");
    assert!(summary(&rig).ends_with("1:00 past its end"));
    if let Some(t) = rig.trial.as_mut() {
        t.crashed = true;
    }
    assert!(summary(&rig).ends_with("trial crashed"));
}

#[test]
fn the_search_counts_solo_limits_found_and_the_current_cores_credited_checks() {
    let mut rig = Rig::new(16, 4);
    assert_eq!(
        rig.progress(),
        [
            "SOLO LIMITS · 4/16 solo limits found".to_owned(),
            "waiting for the next search trial".to_owned()
        ]
    );
    let mut alone = trial(Regime::R1, Some(rig.now), 60);
    alone.condition = Condition::Alone;
    alone.core = Some(core(4));
    alone.offset = Some(off(-20));
    alone.count = Count {
        passed: 1,
        index: 2,
        of: 3,
    };
    rig.trial = Some(alone);
    let [head, requirement] = rig.progress();
    assert_eq!(head, "SOLO LIMITS · 4/16 solo limits found");
    assert_eq!(requirement, "current core searching · 1/3 credited checks");
    assert!(!requirement.contains("-20"), "the offset is the NOW row's");
    rig.cores[4].confirm = Some(crate::model::Confirm {
        offset: off(-20),
        light: 1,
        heavy: 0,
        needed: 3,
    });
    assert!(rig.progress()[1].starts_with("current core confirming"));
}

#[test]
fn phase_one_counts_steps_and_names_the_credited_checks_of_the_step() {
    let [head, requirement] = phase1().progress();
    assert_eq!(head, "PHASE 1 · cycle 2 · 1/3 steps complete");
    assert_eq!(
        requirement,
        "step 2 · 3/4 credited checks · 1 check still needed"
    );
}

#[test]
fn credited_checks_never_exceed_what_the_part_requires() {
    let mut rig = phase1();
    rig.plan = Some(cycle(
        vec![step(Regime::R3, vec![overshooting_part()], true)],
        0,
        false,
    ));
    assert_eq!(rig.progress()[1], "step 1 · 4/4 credited checks");
    let mut short = phase1();
    short.plan = Some(cycle(
        vec![step(Regime::R3, vec![part(3, 1, 2, true)], true)],
        0,
        false,
    ));
    assert_eq!(
        short.progress()[1],
        "step 1 · 2/4 credited checks · 2 checks still needed"
    );
}

#[test]
fn an_open_r7_chain_counts_known_parts_and_says_more_may_follow() {
    let mut rig = phase1();
    rig.plan = Some(cycle(
        vec![step(
            Regime::R7,
            vec![part(1, 0, 1, false), part(1, 0, 0, true)],
            false,
        )],
        0,
        false,
    ));
    assert_eq!(
        rig.progress()[1],
        "step 1 · 2 known parts · more may follow · 0/1 credited checks · 1 check still needed"
    );
    rig.plan = Some(cycle(
        vec![step(
            Regime::R7,
            vec![part(1, 0, 1, false), part(1, 0, 0, true)],
            true,
        )],
        0,
        false,
    ));
    assert_eq!(
        rig.progress()[1],
        "step 1 · part 2 of 2 · 0/1 credited checks · 1 check still needed"
    );
}

#[test]
fn a_repair_names_the_chain_closure_step_and_where_checking_resumes() {
    let mut rig = phase1();
    rig.plan = Some(checking_plan(true));
    rig.resume = Some(StepNo::new(nz(3)));
    let [head, requirement] = rig.progress();
    assert_eq!(
        head,
        "PHASE 1 · repairing after failure · cycle held · 1/3 steps complete"
    );
    assert_eq!(
        requirement,
        "chain closure at step 2 of 3 · checking resumes at step 3 of 3"
    );
    rig.resume = Some(StepNo::new(nz(2)));
    assert_eq!(
        rig.progress()[1],
        "checking resumes at step 2 of 3",
        "closing and resuming at the same step names only where checking resumes"
    );
    rig.plan = Some(cycle(
        vec![
            step(Regime::R1, vec![part(1, 0, 1, false)], true),
            step(Regime::R3, vec![part(1, 0, 1, false)], true),
            step(Regime::R4, vec![part(1, 0, 1, false)], true),
        ],
        0,
        true,
    ));
    rig.resume = None;
    assert_eq!(rig.progress()[1], "every step of the cycle is done");
}

#[test]
fn a_hunt_names_its_group_re_cut_and_the_part_of_this_split() {
    let mut rig = phase1();
    let (mut plan, facts) = hunt(14, 3);
    plan.groups.push(group(3));
    plan.parts = vec![part_of(true), part_of(false)];
    rig.hunt = Some((plan, facts));
    let [head, requirement] = rig.progress();
    assert_eq!(
        head,
        "PHASE 1 · HUNT 14 · group 3 · re-cut 2 · 1/3 steps complete"
    );
    assert_eq!(
        requirement,
        "part 1/2 of this split · total hunt work unknown"
    );
}

fn part_of(running: bool) -> togi_tuner::HuntPart {
    togi_tuner::HuntPart {
        failing: set(&[0]),
        parked: CoreSet::default(),
        trials: 0,
        duration_s: 0,
        group: Some(3),
        outcome: running.then_some(togi_tuner::GroupState::Running),
        running,
    }
}

pub(in crate::frames::dash) fn group(number: u32) -> togi_tuner::HuntGroup {
    togi_tuner::HuntGroup {
        number,
        cores: set(&[0]),
        profile: PerCore::default(),
        stage: togi_journal::HuntStage::Full,
        probe: None,
        held: Vec::new(),
        outcome: togi_tuner::GroupState::Running,
        passed: 0,
        needed: 1,
        carried: false,
        running: true,
    }
}

#[test]
fn a_round_counts_credited_checks_and_keeps_candidates_and_movers_apart() {
    let [head, requirement] = phase2_round().progress();
    assert_eq!(
        head,
        "PHASE 2 · round 3 · 4/8 credited checks · 3 candidates · 2 moving · 2 rounds left if none fails"
    );
    assert_eq!(
        requirement,
        "check 2 of 3 · R1 light · on core 00 · 1/3 credited checks"
    );
}

#[test]
fn the_confirmation_cycle_counts_steps_like_phase_one() {
    let [head, requirement] = confirmation().progress();
    assert_eq!(
        head,
        "PHASE 2 · confirmation cycle · cycle 2 · 1/3 steps complete"
    );
    assert!(requirement.starts_with("step 2 · 3/4 credited checks"));
}

#[test]
fn clean_cycles_count_recorded_cycles_and_the_current_cycles_steps_never_time() {
    let [head, requirement] = clean().progress();
    assert_eq!(
        head,
        "CLEAN CYCLES · 4 clean cycles recorded · cycle 2 · 1/3 steps complete"
    );
    assert!(requirement.starts_with("step 2"));
    for text in [&head, &requirement] {
        assert!(!text.contains('%') && !text.contains("left"), "{text}");
    }
}

#[test]
fn a_stop_keeps_the_last_work_and_gives_its_reason_and_queued_resets() {
    let mut rig = phase1();
    rig.stopped = Some(StopView {
        at: at("2026-10-12T11:30:00Z"),
        reason: ShutdownReason::Signal,
        saved: true,
    });
    rig.cores[2].queued = Some(seq(9));
    let [head, requirement] = rig.progress();
    assert_eq!(head, "STOPPED · PHASE 1 · cycle 2 · 1/3 steps complete");
    assert_eq!(
        requirement, "reset queued for core 02 · BIOS profile: page 6",
        "the stop reason and time are the NOW row's, never repeated"
    );
}

#[test]
fn a_dead_end_and_a_recovery_say_why_without_a_next_forecast() {
    let mut rig = phase1();
    rig.dead_end = Some(DeadEndView {
        at: rig.now,
        condition: DeadEndCondition::ThermalTrip,
        detail: String::new(),
    });
    let [head, requirement] = rig.progress();
    assert!(head.starts_with("DEAD END · PHASE 1"), "{head}");
    assert_eq!(requirement, "the CPU shut down from heat");
    rig.dead_end = None;
    let mut crashed = trial(Regime::R3, Some(rig.now), 60);
    crashed.crashed = true;
    rig.trial = Some(crashed);
    let [head, requirement] = rig.progress();
    assert!(head.starts_with("RECOVERY · PHASE 1"), "{head}");
    assert_eq!(requirement, "trial t1 has no recorded end");
}

#[test]
fn the_progress_rows_fit_whole_clauses_and_never_cut_a_value() {
    let rig = phase1();
    for width in 10..70 {
        let [head, requirement] = phase_progress(&rig.scene(), width).map(|line| plain(&line));
        for text in [&head, &requirement] {
            assert!(text.chars().count() <= width, "{width}: {text:?}");
        }
    }
    let [_, narrow] = phase_progress(&rig.scene(), 24).map(|line| plain(&line));
    assert_eq!(narrow, "step 2 · 3/4 credited");
}

fn next_trial(context: TrialContext) -> TrialPlan {
    let topology = Topology::new(&infos(&[(0, 0), (1, 0)])).expect("a topology");
    let workload = workloads(Regime::R7)[0].id();
    let secs = Secs::new(nz(120));
    let load = Load::Many(set(&[0, 1]));
    TrialPlan {
        regime: Regime::R7,
        workload: WorkloadChoice::Frozen(workload),
        load,
        condition: Condition::Together,
        activity: Activity::Checking,
        profile: Profile::alone(&topology, core(0), off(-5)),
        duration: secs,
        context,
        requirement: Requirement {
            kind: RequirementKind::Cycle,
            class: ClassKey::new(workload, load.cores(), secs),
            window: Window::Live { since: None },
            needed: NonZeroU16::new(1).expect("one"),
            step: None,
            part: None,
        },
        retry: false,
        cause: Vec::new(),
    }
}

pub(in crate::frames::dash) fn failed_on(core_id: Option<u8>) -> TrialEndView {
    TrialEndView {
        id: TrialId::from("t0".to_owned()),
        at: at("2026-10-12T10:00:00Z"),
        regime: Some(Regime::R7),
        cores: set(&[0, 1]),
        outcome: Outcome::Failure,
        signal: None,
        core: core_id.map(core),
        duration: Duration::from_secs(6),
        planned: Some(Duration::from_secs(120)),
        last_sample: None,
        stalled: None,
        tctl_max_c: None,
        voltage_v: None,
    }
}

#[test]
fn a_failure_whose_rerun_runs_next_says_what_the_rerun_must_do_not_that_the_cycle_is_done() {
    let mut rig = Rig::new(2, 2).in_phase(PhaseNo::Two);
    rig.plan = Some(cycle(
        vec![step(Regime::R1, vec![part(1, 0, 1, false)], true)],
        0,
        false,
    ));
    rig.last = Some(failed_on(Some(1)));
    rig.forecast.between = Some(Between {
        next: Some(next_trial(TrialContext::Rerun { failure: seq(3) })),
        ..Between::default()
    });
    assert_eq!(
        rig.progress()[1],
        "failure on core 01 · a clean rerun is needed"
    );
    rig.last = Some(failed_on(None));
    rig.forecast.between = Some(Between {
        next: Some(next_trial(TrialContext::ZeroRerun { failure: seq(3) })),
        ..Between::default()
    });
    assert_eq!(
        rig.progress()[1],
        "failure recorded · a clean rerun with every core at 0 is needed"
    );
    rig.forecast.between = None;
    assert_eq!(rig.progress()[1], "every step of the cycle is done");
}

#[test]
fn a_headline_that_does_not_fit_gives_up_whole_clauses_and_never_ends_in_a_cut() {
    let mut rig = Rig::new(16, 16).in_phase(PhaseNo::One);
    rig.plan = Some(cycle(
        vec![step(Regime::R1, vec![part(1, 0, 0, true)], true); 18],
        0,
        false,
    ));
    rig.phases = None;
    for width in (20..=60).rev() {
        let line = phase_progress(&rig.scene(), width)[0].clone();
        let text = plain(&line);
        assert!(
            !text.contains("..") && !text.contains('…'),
            "{width}: {text}"
        );
        assert!(text.chars().count() <= width, "{width}: {text}");
    }
    let narrow = plain(&phase_progress(&rig.scene(), 40)[0]);
    assert!(narrow.contains("0/18 steps"), "{narrow}");
}

#[test]
fn a_rerun_after_every_step_is_done_says_the_cycle_ends_not_a_step_past_the_last() {
    let mut rig = confirmation();
    rig.plan = Some(cycle(
        vec![
            step(Regime::R1, vec![part(1, 0, 1, false)], true),
            step(Regime::R3, vec![part(1, 0, 1, false)], true),
            step(Regime::R4, vec![part(1, 0, 1, false)], true),
        ],
        3,
        false,
    ));
    rig.trial = Some(TrialView {
        rerun: true,
        ..trial(Regime::R3, Some(rig.now), 60)
    });
    let [head, requirement] = rig.progress();
    assert_eq!(
        head,
        "PHASE 2 · repairing after failure · cycle held · 3/3 steps complete"
    );
    assert_eq!(
        requirement,
        "chain closure at step 3 of 3 · every step is done · the cycle ends once the rerun passes"
    );
}

#[test]
fn a_hunt_or_a_repair_in_the_clean_cycles_keeps_the_clean_cycles_recorded() {
    let mut rig = clean();
    let (mut plan, facts) = hunt(14, 3);
    plan.groups.push(group(3));
    rig.hunt = Some((plan, facts));
    assert_eq!(
        rig.progress()[0],
        "CLEAN CYCLES · HUNT 14 · group 3 · re-cut 2 · 4 clean cycles recorded · 1/3 steps complete"
    );
    rig.hunt = None;
    rig.plan = Some(checking_plan(true));
    assert_eq!(
        rig.progress()[0],
        "CLEAN CYCLES · repairing after failure · cycle held · 4 clean cycles recorded · 1/3 steps complete"
    );
}

#[test]
fn a_hunt_in_a_round_keeps_the_round_and_its_credited_checks() {
    let mut rig = phase2_round();
    let (mut plan, facts) = hunt(14, 3);
    plan.groups.push(group(3));
    rig.hunt = Some((plan, facts));
    assert_eq!(
        rig.progress()[0],
        "PHASE 2 · HUNT 14 · group 3 · re-cut 2 · round 3 · 4/8 credited checks"
    );
}

#[test]
fn a_completed_round_says_every_check_is_credited() {
    let mut rig = phase2_round();
    rig.deepening = Some(round(vec![check(3, 3, false), check(2, 2, false)]));
    let [head, requirement] = rig.progress();
    assert_eq!(
        head,
        "PHASE 2 · round 3 · 5/5 credited checks · 3 candidates · 2 moving · 2 rounds left if none fails"
    );
    assert_eq!(requirement, "every check of the round is credited");
}

/// The model of a tuner session failing together until it asks for a parked hunt trial that `wanted` accepts,
/// with that trial running.
fn parked_hunt_trial(mut script: Script, wanted: impl Fn(&HuntPlan) -> bool) -> Model {
    for _ in 0..300 {
        if let (Ok(Action::RunTrial(plan)), Some(hunt)) =
            (script.shadow.next(), script.shadow.hunt_plan())
            && plan.condition == Condition::Parked
            && wanted(&hunt)
        {
            let (_, trial) = script.intent(&plan);
            script.start(&trial);
            return script.model();
        }
        script.step(|plan| {
            if plan.condition == Condition::Together {
                Outcome::Failure
            } else {
                Outcome::Pass
            }
        });
    }
    panic!("no parked hunt trial was accepted");
}

fn hunt_requirement_of(model: &Model) -> String {
    let zone = TimeZone::UTC;
    let scene = Scene::new(
        model.snapshot(),
        at("2026-10-12T10:00:00Z"),
        &zone,
        Colours::Full,
    );
    plain(&phase_progress(&scene, 200)[1])
}

#[test]
fn a_hunt_says_whether_it_reruns_the_failing_load_or_tests_a_split() {
    let locating = parked_hunt_trial(Script::at_limit(), |_| true);
    assert!(locating.snapshot().hunt.is_some_and(|hunt| hunt.locating()));
    assert_eq!(
        hunt_requirement_of(&locating),
        "locating: the failing load reruns at its failing offsets · total hunt work unknown"
    );

    let cores: Vec<(u8, u8)> = (0..8).map(|id| (id, id / 4)).collect();
    let mut script = Script::new(&cores, &[Start::at_limit(-30, -31); 8]);
    script.append(
        ProfileChange {
            from: None,
            to: (0..8).map(|id| (core(id), off(-30))).collect(),
        }
        .into(),
        &[],
    );
    script.ranking();
    let splitting = parked_hunt_trial(script, |hunt| {
        hunt.groups.iter().any(|group| {
            group.outcome == GroupState::Running
                && !group.cores.is_empty()
                && group.stage != HuntStage::Locate
        })
    });
    assert!(
        !splitting
            .snapshot()
            .hunt
            .is_some_and(|hunt| hunt.locating())
    );
    let requirement = hunt_requirement_of(&splitting);
    assert!(
        requirement.starts_with("part ")
            && requirement.ends_with(" of this split · total hunt work unknown"),
        "{requirement}"
    );
}
