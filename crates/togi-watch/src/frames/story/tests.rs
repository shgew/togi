//! Function-level tests of the narration. Whole-frame assertions belong to the integration of the frames.

use std::collections::{BTreeMap, BTreeSet};
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::LazyLock;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::{Activity, DeadEndCondition, Member, Outcome, ShutdownReason};
use togi_machine::{
    CcdId, Condition, CoreId, CoreSet, ObservedOffset, PerCore, Regime, ResetKind, Signal,
    workloads,
};
use togi_tuner::{
    Between, ClassKey, CyclePart, CyclePlan, CycleStep, Forecast, GroupState, HuntGroup, HuntPart,
    HuntPlan, Lengths, Load, PartNo, PhaseNo, PhasePlan, Profile, Quota, R7CoreStatus, Requirement,
    RerunPlan, Secs, StepNo, Topology, TrialContext, TrialPlan, Uncertainty, Window,
    WorkloadChoice,
};

use super::{
    crash_evidence, known_words, narrator_lines, operation, problem_story, resting_band,
    split_words, starting_story, story, trial_where,
};
use crate::frames::colours::Paint;
use crate::frames::fit::{inert, plain};
use crate::frames::scene::Scene;
use crate::frames::testing::{assert_frame_bounds, draw_checked, rows_of, screen};
use crate::frames::{draw::rows, layout::Layout};
use crate::model::{
    Confirm, CoreState, CoreView, Count, CyclePlace, CycleView, DeadEndView, HuntCause, HuntFacts,
    HuntPlace, HuntShape, HuntView, Model, RecoveryView, SessionView, Snapshot, StopView,
    TrialEndView, TrialView,
};
use crate::screen::{Colours, View};
use crate::tests::kit::{core, infos, nz, off, seq, set};

mod edges;

static UTC: LazyLock<TimeZone> = LazyLock::new(|| TimeZone::UTC);

fn ts(secs: i64) -> Timestamp {
    Timestamp::from_second(1_700_000_000 + secs).expect("a timestamp")
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

struct Fx {
    model: Model,
    session: Option<SessionView>,
    cores: Vec<CoreView>,
    trial: Option<TrialView>,
    last: Option<TrialEndView>,
    forecast: Forecast,
    cycle: Option<(CyclePlan, Vec<Vec<u32>>, Option<StepNo>)>,
    hunt: Option<(HuntPlan, HuntFacts)>,
    phases: Option<PhasePlan>,
    recover: Option<RecoveryView>,
    stopped: Option<StopView>,
    dead_end: Option<DeadEndView>,
    order: Vec<CoreId>,
    shapes: BTreeMap<u32, HuntShape>,
    carried: BTreeSet<togi_journal::Seq>,
    r7: Vec<R7CoreStatus>,
    clean_cycles: u32,
}

impl Fx {
    fn new() -> Self {
        Self {
            model: Model::new(),
            session: Some(SessionView {
                start: ts(0),
                activity: None,
            }),
            cores: Vec::new(),
            trial: None,
            last: None,
            forecast: Forecast::default(),
            cycle: None,
            hunt: None,
            phases: None,
            recover: None,
            stopped: None,
            dead_end: None,
            order: Vec::new(),
            shapes: BTreeMap::new(),
            carried: BTreeSet::new(),
            r7: Vec::new(),
            clean_cycles: 0,
        }
    }

    fn without_session() -> Self {
        Self {
            session: None,
            ..Self::new()
        }
    }

    fn with_cores(mut self, cores: &[(u8, u8)]) -> Self {
        self.cores = cores.iter().map(|&(id, ccd)| core_view(id, ccd)).collect();
        self.order = cores.iter().map(|&(id, _)| core(id)).collect();
        self
    }

    fn snap(&self) -> Snapshot<'_> {
        let mut s = self.model.snapshot();
        s.session = self.session.as_ref();
        s.cores = &self.cores;
        s.trial = self.trial.as_ref();
        s.last = self.last.as_ref();
        s.forecast = &self.forecast;
        s.cycle = self.cycle.as_ref().map(|(plan, hunts, resume)| CycleView {
            plan,
            step_hunts: hunts,
            resume: *resume,
        });
        s.hunt = self
            .hunt
            .as_ref()
            .map(|(plan, facts)| HuntView { plan, facts });
        s.phases = self.phases.as_ref();
        s.recover = self.recover.as_ref();
        s.stopped = self.stopped.as_ref();
        s.dead_end = self.dead_end.as_ref();
        s.order = &self.order;
        s.shapes = &self.shapes;
        s.carried = &self.carried;
        s.r7 = &self.r7;
        s.clean_cycles = self.clean_cycles;
        s
    }

    fn scene(&self, now: Timestamp) -> Scene<'_> {
        Scene::new(self.snap(), now, &UTC, Colours::None)
    }
}

fn core_view(id: u8, ccd: u8) -> CoreView {
    CoreView {
        id: core(id),
        ccd: CcdId::try_from(ccd).unwrap(),
        state: CoreState::AtLimit,
        queued: None,
        applied: Some(ObservedOffset(0)),
        profile: off(0),
        solo: None,
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
        evidence: crate::model::CoreEvidence::default(),
    }
}

fn trial(regime: Regime, condition: Condition, cores: &[u8]) -> TrialView {
    TrialView {
        id: "t1".to_owned().into(),
        regime,
        workload: workloads(regime)[0].id(),
        cores: set(cores),
        parked: CoreSet::default(),
        idle: CoreSet::default(),
        condition,
        activity: Some(Activity::Checking),
        profile: PerCore::default(),
        started: Some(ts(1000)),
        crashed: false,
        duration: secs(120),
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

fn in_cycle(mut trial: TrialView, cycle: u32, step: u32, part: u32, parts: u32) -> TrialView {
    trial.cycle = Some(CyclePlace {
        cycle,
        step: Some(step),
        part: Some(part),
        parts,
    });
    trial
}

fn alone(regime: Regime, id: u8, offset: i64) -> TrialView {
    let mut trial = trial(regime, Condition::Alone, &[id]);
    trial.core = Some(core(id));
    trial.offset = Some(off(offset));
    trial
}

fn part(cores: &[u8], ccd: Option<u8>, full: bool, running: bool) -> CyclePart {
    CyclePart {
        cores: set(cores),
        ccd: ccd.map(|ccd| CcdId::try_from(ccd).unwrap()),
        full,
        partial: !full,
        lengths: Some(Lengths {
            short: Quota {
                count: 1,
                secs: 120,
            },
            long: None,
        }),
        passed: 0,
        failed: 0,
        done: false,
        running,
    }
}

fn step(regime: Regime, parts: Vec<CyclePart>) -> CycleStep {
    CycleStep {
        regime,
        workload: workloads(regime)[0].id(),
        parts,
        chains_complete: false,
        done: false,
    }
}

fn cycle_plan(number: u32, steps: Vec<CycleStep>, current: usize) -> CyclePlan {
    CyclePlan {
        number: NonZeroU32::new(number),
        open: true,
        steps,
        current,
        paused: false,
    }
}

fn step_no(n: u32) -> StepNo {
    StepNo::new(nz(n))
}

fn plan(
    regime: Regime,
    load: Load,
    condition: Condition,
    context: TrialContext,
    length: u32,
) -> TrialPlan {
    let topology = Topology::new(&infos(&[(0, 0), (1, 0), (2, 1), (3, 1)])).expect("a topology");
    let workload = workloads(regime)[0].id();
    let secs = Secs::new(nz(length));
    TrialPlan {
        regime,
        workload: WorkloadChoice::Frozen(workload),
        load,
        condition,
        activity: Activity::Checking,
        profile: Profile::alone(&topology, core(3), off(-16)),
        duration: secs,
        requirement: Requirement {
            context,
            class: ClassKey::new(workload, load.cores(), secs),
            window: Window::Live { since: None },
            needed: NonZeroU16::new(1).expect("one"),
        },
        retry: false,
        cause: Vec::new(),
    }
}

fn cycle_ctx(cycle: u32, step: u32) -> TrialContext {
    TrialContext::Cycle {
        cycle: nz(cycle),
        step: step_no(step),
        part: PartNo::new(nz(1)),
    }
}

fn many(cores: &[u8]) -> Load {
    Load::Many(set(cores))
}

fn hunt_plan() -> HuntPlan {
    HuntPlan {
        number: 1,
        regime: Regime::R7,
        failure: seq(1),
        trial: None,
        candidates: set(&[0, 2]),
        cut: 0,
        parts: Vec::new(),
        groups: Vec::new(),
        probes: Vec::new(),
        rerun: RerunPlan {
            regime: Regime::R7,
            cores: CoreSet::default(),
            lengths: Lengths {
                short: Quota {
                    count: 1,
                    secs: 600,
                },
                long: None,
            },
        },
    }
}

fn cause() -> HuntCause {
    HuntCause {
        at: None,
        trial: None,
        end: None,
        signal: None,
        core: None,
        known: false,
        carried: false,
        regime: None,
        rerun_of: false,
        trial_number: 1,
    }
}

fn text(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
    lines.iter().map(plain).collect()
}

fn said(story: &super::Story) -> String {
    story.lines.join("\n")
}

fn end_view(outcome: Outcome, signal: Option<Signal>) -> TrialEndView {
    TrialEndView {
        id: "t0".to_owned().into(),
        at: ts(900),
        regime: Some(Regime::R1),
        cores: set(&[0, 1]),
        outcome,
        signal,
        core: None,
        duration: secs(600),
        planned: Some(secs(600)),
        last_sample: None,
        stalled: None,
        tctl_max_c: None,
        voltage_v: None,
    }
}

mod the_story {
    use super::*;

    #[test]
    fn no_trial_yet_does_not_claim_one_ended() {
        let mut fx = Fx::new();
        let first = story(&fx.scene(ts(10)));
        assert!(said(&first).contains("No trial has run yet"));
        assert!(!said(&first).contains("has ended"));
        fx.last = Some(end_view(Outcome::Pass, None));
        let after = story(&fx.scene(ts(10)));
        assert!(said(&after).contains("The last trial has ended"));
        assert_eq!(after.label, "TOGI");
    }

    #[test]
    fn a_missing_journal_is_no_session_yet() {
        let fx = Fx::without_session();
        let said = story(&fx.scene(ts(0)));
        assert_eq!(said.label, "NO SESSION YET");
        assert!(said.lines[0].contains("togi run"));
        assert!(said.brief.is_empty());
    }

    #[test]
    fn starting_checks_the_machine_before_the_first_trial() {
        let starting = starting_story();
        assert_eq!(starting.label, "STARTING");
        assert!(starting.lines[0].starts_with("Checking the machine and the journal"));
    }

    #[test]
    fn an_unreadable_journal_is_not_a_missing_one() {
        let error = std::io::Error::other("journal line 1: invalid character 'g'");
        let live = problem_story(&error, false);
        let once = problem_story(&error, true);
        for story in [&live, &once] {
            assert_eq!(story.label, "JOURNAL UNREADABLE");
            assert_eq!(
                story.lines[0],
                "can't read journal: journal line 1: invalid character 'g'"
            );
            assert_eq!(story.paint, Paint::Bad);
        }
        assert!(live.lines[1].contains("I'll retry when the journal changes"));
        assert_eq!(once.lines.len(), 1, "a one-frame print promises no retry");
        let missing = story(&Fx::without_session().scene(ts(0)));
        assert_ne!(missing.label, live.label);
        assert!(!missing.lines[0].contains("can't read journal"));
    }

    #[test]
    fn the_error_text_is_escaped_visibly_at_output() {
        let error = std::io::Error::other("bad \u{1b}[31m byte\u{7}");
        let problem = problem_story(&error, true);
        assert!(
            problem.lines[0].contains('\u{1b}'),
            "the story keeps the raw text"
        );
        let drawn = text(&narrator_lines(&problem, 120, 4, false, Colours::None)).join("");
        assert!(!drawn.contains('\u{1b}') && !drawn.contains('\u{7}'));
        assert!(drawn.contains(inert("\u{1b}[31m").as_ref()));
    }

    #[test]
    fn a_trial_intent_does_not_claim_the_offsets_are_set() {
        let mut fx = Fx::new();
        let mut waiting = trial(Regime::R6, Condition::Together, &[0, 1]);
        waiting.started = None;
        fx.trial = Some(waiting);
        let said = story(&fx.scene(ts(5)));
        let all = said.lines.join(" ");
        assert!(all.contains("intent is recorded") && all.contains("being applied"));
        assert!(!all.contains("offsets are set"));
        assert!(
            fx.scene(ts(5)).hold.is_none(),
            "an unstarted idle trial holds nothing"
        );
    }

    #[test]
    fn a_crashed_trial_is_not_narrated_as_running() {
        let mut fx = Fx::new();
        let mut crashed = alone(Regime::R1, 0, -25);
        crashed.crashed = true;
        crashed.id = "trial-B".to_owned().into();
        fx.trial = Some(crashed);
        let said = story(&fx.scene(ts(5000)));
        assert_eq!(said.label, "CRASHED");
        assert_eq!(said.paint, Paint::Hunt);
        assert!(said.lines[0].contains("Trial trial-B crashed"));
        assert!(said.lines[0].contains("togi run records the crash"));
        for bad in [
            "I'm finding",
            "I'm confirming",
            "past its planned end",
            "left",
        ] {
            assert!(!said.lines[0].contains(bad), "{bad}");
        }
        let band = resting_band(&fx.scene(ts(5000)));
        assert_eq!(band.title, "CRASHED, NOT YET RECOVERED");
        assert!(band.lines[0].starts_with("trial trial-B · "));
        assert!(band.lines[0].contains("started "));
        assert!(band.lines[2].contains("togi run records the crash"));
    }

    #[test]
    fn a_prior_recovery_does_not_mask_a_later_boot_crash() {
        let mut fx = Fx::new();
        let mut crashed = alone(Regime::R1, 0, -20);
        crashed.crashed = true;
        crashed.id = "trial-B".to_owned().into();
        crashed.started = None;
        fx.trial = Some(crashed);
        fx.recover = Some(RecoveryView {
            crash_at: ts(50),
            reset: None,
            trial: Some(alone(Regime::R1, 0, -25)),
            end: Some(end_view(Outcome::Failure, Some(Signal::Crash))),
        });
        let scene = fx.scene(ts(100));
        assert_eq!(story(&scene).label, "CRASHED");
        let band = resting_band(&scene);
        assert_eq!(band.title, "CRASHED, NOT YET RECOVERED");
        assert!(band.lines.join(" ").contains("trial trial-B"));
        assert!(
            !band.lines[0].contains(" · started "),
            "an unstarted trial has no start"
        );
    }

    #[test]
    fn a_command_shutdown_does_not_mask_an_unrecovered_crash() {
        let mut fx = Fx::new();
        let mut crashed = alone(Regime::R1, 0, -20);
        crashed.crashed = true;
        fx.trial = Some(crashed);
        fx.stopped = Some(StopView {
            at: ts(60),
            reason: ShutdownReason::Command,
            saved: false,
        });
        let scene = fx.scene(ts(100));
        assert_eq!(story(&scene).label, "CRASHED");
        assert_eq!(resting_band(&scene).title, "CRASHED, NOT YET RECOVERED");
    }

    #[test]
    fn a_trial_past_its_end_says_how_long_without_claiming_a_crash() {
        let mut fx = Fx::new();
        fx.trial = Some(alone(Regime::R1, 0, -25));
        let before = story(&fx.scene(ts(1000 + 119)));
        assert!(!before.lines[0].contains("past"));
        let past = story(&fx.scene(ts(1000 + 120 + 42)));
        assert_eq!(
            past.lines[0],
            "0:42 past its planned end, no trial end recorded. Its deadline or watchdog may still be running."
        );
        assert_eq!(past.brief, "");
        for bad in ["crashed", "togi run records", "nothing recorded since"] {
            assert!(!past.lines[0].contains(bad), "{bad}");
        }
    }

    #[test]
    fn an_idle_trial_holds_the_story_until_its_end_is_recorded() {
        let mut fx = Fx::new();
        let mut idle = in_cycle(trial(Regime::R6, Condition::Together, &[0, 1]), 1, 2, 1, 1);
        idle.duration = secs(900);
        fx.trial = Some(idle);
        let first = story(&fx.scene(ts(1000 + 1)));
        assert_eq!(first.label, "CYCLE 1");
        assert!(first.lines[0].starts_with("Step 2 leaves every core idle for 15 minutes"));
        assert!(
            first.lines[1].contains("holds still while the idle trial is in flight"),
            "{:?}",
            first.lines
        );
        for at in [1000 + 899, 1000 + 900 + 42, 1000 + 7200] {
            assert_eq!(
                story(&fx.scene(ts(at))),
                first,
                "the idle story never moves with the clock"
            );
        }
        assert!(
            first
                .lines
                .iter()
                .all(|line| !line.contains("left") && !line.contains("past its"))
        );
        let mut crashed = fx.trial.clone().expect("a trial");
        crashed.crashed = true;
        fx.trial = Some(crashed);
        assert!(
            fx.scene(ts(1000 + 3000)).hold.is_none(),
            "a crashed idle trial holds nothing"
        );
        assert_eq!(story(&fx.scene(ts(1000 + 3000))).label, "CRASHED");
    }

    #[test]
    fn a_stopped_session_says_why_and_whether_the_offsets_were_restored() {
        let mut fx = Fx::new();
        fx.stopped = Some(StopView {
            at: ts(100),
            reason: ShutdownReason::Cycles,
            saved: true,
        });
        let said = story(&fx.scene(ts(200)));
        assert_eq!(said.label, "STOPPED");
        assert!(said.lines[0].starts_with(
            "The requested clean cycles are complete, so I restored baseline or shallower offsets"
        ));
        let band = resting_band(&fx.scene(ts(200)));
        assert_eq!(band.title, "STOPPED");
        assert_eq!(
            band.lines,
            [format!(
                "{} · requested clean cycles completed",
                crate::frames::scene::wall_second(&UTC, ts(100))
            )]
        );
        for (reason, saved, want) in [
            (
                ShutdownReason::FirstResult,
                false,
                "stopped at the first result as asked",
            ),
            (
                ShutdownReason::Cycles,
                false,
                "The requested clean cycles are complete, so I've stopped",
            ),
            (
                ShutdownReason::Signal,
                true,
                "I restored baseline or shallower offsets before stopping",
            ),
            (
                ShutdownReason::Signal,
                false,
                "I've stopped. Everything I learned is in the journal",
            ),
        ] {
            fx.stopped = Some(StopView {
                at: ts(100),
                reason,
                saved,
            });
            assert!(said_of(&fx).contains(want), "{reason:?} {saved}");
        }
    }

    fn said_of(fx: &Fx) -> String {
        story(&fx.scene(ts(200))).lines.join(" ")
    }

    #[test]
    fn a_dead_end_preserves_the_recorded_cause() {
        for (condition, detail, wrong) in [
            (
                DeadEndCondition::NoEvidence,
                "kernel log unreadable: permission denied",
                "installed",
            ),
            (
                DeadEndCondition::Containment,
                "termination unconfirmed",
                "termination confirmed",
            ),
            (
                DeadEndCondition::ThermalTrip,
                "firmware reported a thermal trip",
                "wrong result",
            ),
            (
                DeadEndCondition::FailureAtZero,
                "core 03 failed at 0",
                "installed",
            ),
        ] {
            let mut fx = Fx::new();
            fx.dead_end = Some(DeadEndView {
                at: ts(10),
                condition,
                detail: detail.to_owned(),
            });
            let scene = fx.scene(ts(20));
            let band = resting_band(&scene);
            assert_eq!(band.title, "DEAD END");
            assert_eq!(band.paint, Paint::Bad);
            assert_eq!(
                band.lines,
                [format!(
                    "{}: {detail}",
                    condition.as_str().replace('_', " ")
                )]
            );
            assert!(!band.lines[0].contains(wrong));
            let said = story(&scene);
            assert_eq!(said.label, "DEAD END");
            assert_eq!(said.paint, Paint::Bad);
        }
        let mut fx = Fx::new();
        fx.dead_end = Some(DeadEndView {
            at: ts(1),
            condition: DeadEndCondition::Smu,
            detail: String::new(),
        });
        fx.stopped = Some(StopView {
            at: ts(2),
            reason: ShutdownReason::DeadEnd,
            saved: true,
        });
        assert_eq!(resting_band(&fx.scene(ts(3))).lines.len(), 1);
    }

    #[test]
    fn a_dead_end_detail_stays_raw_in_the_band_and_out_of_the_narration() {
        let mut fx = Fx::new();
        fx.dead_end = Some(DeadEndView {
            at: ts(1),
            condition: DeadEndCondition::NoEvidence,
            detail: "bad \u{1b}[2J".to_owned(),
        });
        let band = resting_band(&fx.scene(ts(2)));
        assert!(band.lines[0].contains('\u{1b}'));
        let narrated = said(&story(&fx.scene(ts(2))));
        assert!(!narrated.contains("bad") && !narrated.contains('\u{1b}'));
    }

    #[test]
    fn deepening_checks_say_how_they_load() {
        let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0)]);
        let mut round = alone(Regime::R1, 0, -11);
        round.round = Some(1);
        round.activity = Some(Activity::Deepening);
        fx.trial = Some(round.clone());
        fx.phases = Some(PhasePlan {
            phase: PhaseNo::Two,
            phase1_end: None,
            concluded: None,
            confirming: false,
            rounds_left: 2,
            round: Some(1),
            candidates: Vec::new(),
        });
        let scene = fx.scene(ts(1010));
        let said = story(&scene);
        assert_eq!(said.label, "PHASE 2 · ROUND 1");
        assert!(said.lines[0].contains("Core 00 runs alone at its proposed -11"));
        assert!(
            !said
                .lines
                .join(" ")
                .contains("solo limit, one turn at a time")
        );
        assert_eq!(
            plain(&operation(&scene, &round)),
            "PHASE 2 · ROUND 1 · CORE 00 AT -11 · LIGHT"
        );
        let together = {
            let mut t = trial(Regime::R7, Condition::Together, &[0, 1]);
            t.round = Some(1);
            t
        };
        fx.trial = Some(together.clone());
        let scene = fx.scene(ts(1010));
        let said = story(&scene);
        assert_eq!(said.label, "PHASE 2 · ROUND 1");
        assert!(said.lines[0].starts_with("Round 1 checks again the offsets kept from the round that ended unpassed; 2 rounds left if no round fails"));
        assert!(!said.lines.join(" ").contains(" alone at "));
        assert_eq!(plain(&operation(&scene, &together)), "PHASE 2 · ROUND 1");
    }

    #[test]
    fn a_round_names_the_cores_it_moves() {
        let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0), (2, 1)]);
        let mut together = trial(Regime::R7, Condition::Together, &[0, 1, 2]);
        together.round = Some(2);
        fx.trial = Some(together);
        let candidate = |id: u8, moving| togi_tuner::Candidate {
            core: core(id),
            offset: off(-10),
            solo_limit: None,
            gap: 1,
            carried: false,
            moving,
        };
        fx.phases = Some(PhasePlan {
            phase: PhaseNo::Two,
            phase1_end: None,
            concluded: None,
            confirming: false,
            rounds_left: 1,
            round: Some(2),
            candidates: vec![candidate(0, true), candidate(1, false), candidate(2, true)],
        });
        let said = story(&fx.scene(ts(1010)));
        assert!(said.lines[0].starts_with("Round 2 moves cores 00, 02 one count toward their solo limits; 1 round left if no round fails, then one full cycle."));
        assert_eq!(said.brief, "Checking round 2's proposed profile.");
    }

    #[test]
    fn a_search_trial_says_what_it_is_finding_or_confirming() {
        let mut fx = Fx::new().with_cores(&[(0, 0), (3, 1)]);
        fx.trial = Some(alone(Regime::R1, 3, -16));
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(said.label, "SOLO LIMITS");
        assert_eq!(
            said.lines[0],
            "I'm finding core 03's solo limit, one turn at a time, now at -16."
        );
        assert_eq!(said.brief, said.lines[0]);
        let tight = text(&narrator_lines(&said, 60, 2, false, Colours::None)).join(" ");
        assert!(
            tight.contains("now at -16.") && !tight.contains("..."),
            "{tight}"
        );
        assert_eq!(
            plain(&operation(
                &fx.scene(ts(1010)),
                fx.trial.as_ref().expect("a trial")
            )),
            "SEARCH CORE 03 AT -16 · LIGHT"
        );
        fx.cores[1].confirm = Some(Confirm {
            offset: off(-15),
            light: 1,
            heavy: 0,
            needed: 5,
        });
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(
            said.lines[0],
            "-15 is core 03's candidate solo limit, and I'm confirming it."
        );
        fx.cores[1].fail = Some(off(-16));
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(
            said.lines[0],
            "Core 03 failed at -16, so -15 is its candidate solo limit, and I'm confirming it."
        );
        assert_eq!(
            plain(&operation(
                &fx.scene(ts(1010)),
                fx.trial.as_ref().expect("a trial")
            )),
            "CONFIRM CORE 03 AT -16 · LIGHT"
        );
    }

    #[test]
    fn a_partial_explains_that_it_is_ordinary_evidence() {
        let mut fx = Fx::new();
        let mut partial = in_cycle(trial(Regime::R7, Condition::Together, &[1]), 1, 1, 1, 2);
        partial.partial = true;
        fx.trial = Some(partial.clone());
        let said = story(&fx.scene(ts(1010)));
        let all = said.lines.join("\n");
        for want in [
            "top-requester groups",
            "A backoff re-derives it",
            "an unchanged loaded set keeps its passes",
            "Passes and failures count as ordinary evidence",
        ] {
            assert!(all.contains(want), "lost {want}: {all}");
        }
        assert_eq!(
            said.brief,
            "Step 1, R7 partial loads 01; passes and failures count."
        );
        let op = plain(&operation(&fx.scene(ts(1010)), &partial));
        assert!(
            op.contains("PARTIAL") && !op.contains("RECORD ONLY"),
            "{op}"
        );
    }

    #[test]
    fn a_checking_step_says_where_it_runs_and_what_counts() {
        let mut fx = Fx::new();
        let checking = in_cycle(
            trial(Regime::R2, Condition::Together, &[0, 1, 2, 3]),
            2,
            3,
            2,
            4,
        );
        fx.trial = Some(checking);
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(said.label, "CYCLE 2");
        assert_eq!(
            said.lines[0],
            format!(
                "Step 3 is R2 heavy vector with {}.",
                crate::words::workload_display(workloads(Regime::R2)[0].id())
            )
        );
        assert_eq!(said.lines[1], "Part 2 of 4 loads 00-03 for 2 minutes.");
        assert_eq!(
            said.brief,
            "Step 3, R2 heavy vector: part 2 of 4 loads 00-03 for 2 minutes."
        );
        fx.clean_cycles = 2;
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(
            said.lines[1],
            "2 clean cycles count for this profile. Passing trials cannot prove it will never fail."
        );
        fx.clean_cycles = 1;
        assert!(
            story(&fx.scene(ts(1010))).lines[1]
                .starts_with("1 clean cycle counts for this profile.")
        );
        fx.clean_cycles = 0;
        fx.phases = Some(PhasePlan {
            phase: PhaseNo::One,
            phase1_end: None,
            concluded: None,
            confirming: false,
            rounds_left: 0,
            round: None,
            candidates: Vec::new(),
        });
        fx.trial = Some(in_cycle(
            trial(Regime::R1, Condition::Together, &[0, 1]),
            1,
            1,
            1,
            1,
        ));
        assert!(
            story(&fx.scene(ts(1010))).lines[1]
                .contains("Phase 1's cycle: its pass confirms the first BIOS profile.")
        );
    }

    #[test]
    fn phase_2s_confirmation_cycle_says_its_pass_concludes_phase_2() {
        let mut fx = Fx::new();
        fx.phases = Some(PhasePlan {
            phase: PhaseNo::Two,
            phase1_end: None,
            concluded: None,
            confirming: true,
            rounds_left: 0,
            round: None,
            candidates: Vec::new(),
        });
        fx.trial = Some(in_cycle(
            trial(Regime::R1, Condition::Together, &[0, 1]),
            3,
            1,
            1,
            1,
        ));
        assert_eq!(
            story(&fx.scene(ts(1010))).lines[1],
            "Every core runs at its profile offset. Phase 2's confirmation cycle: its pass concludes phase 2."
        );
    }

    #[test]
    fn an_all_zero_rerun_is_named_for_what_it_decides() {
        let mut fx = Fx::new();
        let mut rerun = trial(Regime::R6, Condition::Parked, &[0, 1, 2]);
        rerun.rerun = true;
        fx.trial = Some(rerun.clone());
        let scene = fx.scene(ts(1010));
        let said = story(&scene);
        assert_eq!(said.label, "RERUN AT OFFSET 0");
        assert_eq!(plain(&operation(&scene, &rerun)), "RERUN AT OFFSET 0");
        assert!(said.lines[0].starts_with("A failure at offset 0 would stop tuning."));
        assert_eq!(
            said.brief,
            "The failed load runs again with every core at offset 0; tuning stops if it fails."
        );
    }

    #[test]
    fn a_rerun_names_the_step_checking_resumes_at() {
        let mut fx = Fx::new();
        let mut rerun = in_cycle(trial(Regime::R7, Condition::Together, &[0, 1]), 1, 1, 1, 1);
        rerun.rerun = true;
        fx.trial = Some(rerun.clone());
        let steps = vec![
            step(Regime::R1, vec![part(&[0, 1], None, true, false)]),
            step(Regime::R2, vec![part(&[0, 1], None, true, false)]),
            step(Regime::R7, vec![part(&[0, 1], None, true, false)]),
        ];
        fx.cycle = Some((
            cycle_plan(1, steps, 0),
            vec![Vec::new(); 3],
            Some(step_no(3)),
        ));
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(said.label, "RERUN");
        assert_eq!(
            said.lines[1],
            "Cycle 1 waits at step 3 until the rerun passes."
        );
        assert!(
            said.brief
                .ends_with("; cycle 1 waits at step 3 until the rerun passes.")
        );
        assert_eq!(
            plain(&operation(&fx.scene(ts(1010)), &rerun)),
            "RERUN AFTER BACKOFF"
        );
        fx.cycle.as_mut().expect("a cycle").2 = None;
        assert_eq!(
            story(&fx.scene(ts(1010))).lines[1],
            "Cycle 1 waits at step 1 until the rerun passes."
        );
        fx.cycle.as_mut().expect("a cycle").0.current = 3;
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(
            said.lines.len(),
            1,
            "a cycle with every step done waits at no step"
        );
        assert_eq!(said.brief, "The failed load runs again on the new profile.");
    }

    #[test]
    fn a_rerun_that_completes_the_cycle_names_no_step_of_the_next() {
        let built = togi_watchtest::cycle_completed_by_rerun().expect("the rerun journals");
        assert!(!built.running.is_empty());
        for (n, events) in built.running.iter().enumerate() {
            let model = crate::model::project(events);
            let snap = model.snapshot();
            let scene = Scene::new(snap, ts(5000), &UTC, Colours::None);
            let said = story(&scene);
            assert_eq!(said.label, "RERUN", "rerun {}", n + 1);
            assert!(
                said.lines
                    .iter()
                    .any(|line| line == "Cycle 1 waits at step 2 until the rerun passes."),
                "rerun {}: {:?}",
                n + 1,
                said.lines
            );
            assert!(
                said.lines
                    .iter()
                    .all(|line| !line.contains("waits at step 1 "))
            );
        }
        for (what, events) in [("passed", &built.passed), ("ended", &built.ended)] {
            let model = crate::model::project(events);
            let view = model.snapshot().cycle.expect("cycle 1");
            assert_eq!(view.plan.number, NonZeroU32::new(1), "{what}");
            assert_eq!(
                (view.resume_step() + 1).min(view.plan.steps.len()),
                2,
                "{what}: the stage line names the step checking resumes at"
            );
        }
    }

    #[test]
    fn a_hunt_cause_brief_agrees_with_the_named_core() {
        let mut crashed = trial(Regime::R7, Condition::Together, &[0, 1]);
        crashed.duration = secs(600);
        for (trial, signal) in [
            (Some(crashed.clone()), Some(Signal::Crash)),
            (Some(crashed.clone()), Some(Signal::ComputationError)),
            (None, Some(Signal::Crash)),
        ] {
            let mut c = cause();
            c.trial = trial;
            c.signal = signal;
            c.core = Some(core(9));
            let (_, named, brief) = super::super::hunt_cause_story(&c);
            assert!(named.starts_with("Core 09 was named"), "{named}");
            assert!(brief.ends_with("; core 09 named."), "{brief}");
            c.core = None;
            let (_, named, brief) = super::super::hunt_cause_story(&c);
            assert!(named.starts_with("No core was named"), "{named}");
            assert!(brief.ends_with("; no core named."), "{brief}");
        }
    }

    #[test]
    fn a_hunt_cause_names_what_failed_and_how_late() {
        let mut crashed = trial(Regime::R7, Condition::Together, &[0, 1]);
        crashed.duration = secs(600);
        let mut c = cause();
        c.trial = Some(crashed.clone());
        c.signal = Some(Signal::ComputationError);
        let (sentence, _, brief) = super::super::hunt_cause_story(&c);
        assert_eq!(
            sentence,
            "The 10-minute all-core trial on 00 01 ended with wrong result."
        );
        assert!(brief.starts_with("Wrong result in R7 all-core on 00 01"));
        c.signal = Some(Signal::Crash);
        c.rerun_of = true;
        let mut end = end_view(Outcome::Failure, Some(Signal::Crash));
        end.last_sample = Some(secs(500));
        end.planned = Some(secs(600));
        c.end = Some(end);
        let (sentence, _, brief) = super::super::hunt_cause_story(&c);
        assert_eq!(
            sentence,
            "The machine crashed late in the rerun of the 10-minute all-core trial on 00 01 and rebooted."
        );
        assert!(
            brief.starts_with("Crashed late in R7 all-core on 00 01"),
            "{brief}"
        );
        c.end = None;
        let (sentence, _, brief) = super::super::hunt_cause_story(&c);
        assert!(
            sentence.starts_with("The machine crashed during the rerun"),
            "{sentence}"
        );
        assert!(brief.starts_with("Crashed in R7"), "{brief}");
        let mut idle = cause();
        idle.signal = Some(Signal::Crash);
        assert_eq!(
            super::super::hunt_cause_story(&idle).0,
            "The machine crashed while idle, with no trial running, and rebooted."
        );
        idle.signal = Some(Signal::ComputationError);
        assert!(
            super::super::hunt_cause_story(&idle)
                .0
                .contains("wrong result")
        );
    }

    #[test]
    fn a_carried_idle_failure_cause_is_not_a_trial() {
        let mut c = cause();
        c.known = true;
        c.carried = true;
        let words = known_words(&c);
        assert!(
            !words.contains("trial") && words.contains("idle failure"),
            "{words}"
        );
        assert_eq!(
            words,
            "an idle failure is already recorded at these offsets, carried from an earlier session"
        );
        c.regime = Some(Regime::R7);
        assert_eq!(
            known_words(&c),
            "R7 all-core already failed at these offsets in a carried trial"
        );
        c.carried = false;
        assert_eq!(
            known_words(&c),
            "R7 all-core already failed at these offsets"
        );
        let (sentence, _, brief) = super::super::hunt_cause_story(&c);
        assert_eq!(sentence, "R7 all-core already failed at these offsets.");
        assert_eq!(brief, sentence);
    }

    fn hunting(parked_zero: bool, parts: Vec<HuntPart>) -> Fx {
        let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0), (2, 1)]);
        let mut plan = hunt_plan();
        plan.parts = parts;
        let mut facts = HuntFacts {
            started: Some(ts(990)),
            cause: cause(),
            parking: parked_zero.then(|| crate::model::Parking {
                zero: set(&[1, 2]),
                ..crate::model::Parking::default()
            }),
            group_signals: Vec::new(),
        };
        facts.cause.signal = Some(Signal::ComputationError);
        fx.hunt = Some((plan, facts));
        fx
    }

    fn running_part(failing: &[u8], parked: &[u8]) -> HuntPart {
        HuntPart {
            failing: set(failing),
            parked: set(parked),
            trials: 1,
            duration_s: 120,
            group: Some(1),
            outcome: Some(GroupState::Running),
            running: true,
        }
    }

    #[test]
    fn a_split_parks_at_zero_only_when_the_profile_does() {
        let mut fx = hunting(true, vec![running_part(&[0], &[1, 2])]);
        let mut running = trial(Regime::R7, Condition::Together, &[0]);
        running.profile = crate::tests::kit::offsets(&[-10, 0, -30]);
        fx.trial = Some(running.clone());
        let scene = fx.scene(ts(1010));
        let hunt = scene.s.hunt.expect("a hunt");
        let words = split_words(&scene, hunt, &running);
        assert!(
            !words.contains("at 0"),
            "core 02 stays at -30, yet the split says {words:?}"
        );
        running.profile = crate::tests::kit::offsets(&[-10, 0, 0]);
        fx.trial = Some(running.clone());
        let scene = fx.scene(ts(1010));
        let hunt = scene.s.hunt.expect("a hunt");
        assert_eq!(
            split_words(&scene, hunt, &running),
            "00 at their failing offsets, 01 02 parked at 0"
        );
    }

    #[test]
    fn a_hunt_without_a_running_part_says_how_the_rest_is_parked() {
        let mut fx = hunting(false, Vec::new());
        let running = trial(Regime::R7, Condition::Together, &[0]);
        fx.trial = Some(running.clone());
        let scene = fx.scene(ts(1010));
        let hunt = scene.s.hunt.expect("a hunt");
        assert_eq!(
            split_words(&scene, hunt, &running),
            "part of 00 02 at its offsets, the rest parked"
        );
    }

    #[test]
    fn a_hunt_story_gives_its_cause_then_how_it_splits() {
        let mut fx = hunting(true, vec![running_part(&[0], &[1, 2])]);
        let mut running = trial(Regime::R7, Condition::Together, &[0]);
        running.parked = set(&[1, 2]);
        running.profile = crate::tests::kit::offsets(&[-10, 0, 0]);
        running.hunt = Some(HuntPlace {
            hunt: 1,
            group: 1,
            part: Some(1),
            parts: 2,
            cut: 1,
            probe: None,
        });
        fx.trial = Some(running.clone());
        fx.hunt.as_mut().expect("a hunt").1.cause.trial =
            Some(trial(Regime::R7, Condition::Together, &[0, 2]));
        let scene = fx.scene(ts(1010));
        let said = story(&scene);
        assert_eq!(said.label, "HUNT 1");
        assert_eq!(said.paint, Paint::Hunt);
        assert!(
            said.lines[0]
                .starts_with("The 2-minute all-core trial on 00 02 ended with wrong result.")
        );
        assert!(
            said.lines[1].starts_with(
                "No core was named: I rerun that load with 00 at their failing offsets"
            )
        );
        assert!(
            said.brief.ends_with("Rerunning it with 01 02 parked."),
            "{}",
            said.brief
        );
        assert_eq!(plain(&operation(&scene, &running)), "HUNT 1 · PART 1 OF 2");
        running.hunt = Some(HuntPlace {
            hunt: 1,
            group: 3,
            part: Some(2),
            parts: 3,
            cut: 3,
            probe: None,
        });
        assert_eq!(
            plain(&operation(&scene, &running)),
            "HUNT 1 · RE-CUT 2 · PART 2 OF 3"
        );
        running.hunt = Some(HuntPlace {
            hunt: 1,
            group: 3,
            part: None,
            parts: 0,
            cut: 0,
            probe: Some(Member {
                core: core(2),
                offset: off(-12),
            }),
        });
        assert_eq!(
            plain(&operation(&scene, &running)),
            "HUNT 1 · GROUP 3 · CORE 02 AT -12"
        );
        running.hunt = Some(HuntPlace {
            hunt: 1,
            group: 3,
            part: None,
            parts: 0,
            cut: 0,
            probe: None,
        });
        assert_eq!(plain(&operation(&scene, &running)), "HUNT 1 · GROUP 3");
    }

    #[test]
    fn a_member_probe_names_the_group_and_the_member() {
        let mut fx = hunting(false, Vec::new());
        let (plan, facts) = fx.hunt.as_mut().expect("a hunt");
        plan.groups.push(HuntGroup {
            number: 1,
            cores: set(&[0, 2]),
            profile: PerCore::default(),
            stage: togi_journal::HuntStage::Part,
            probe: None,
            held: Vec::new(),
            outcome: GroupState::Failed,
            passed: 0,
            needed: 5,
            carried: false,
            running: false,
        });
        facts.group_signals.push((1, Signal::Crash));
        let mut probing = trial(Regime::R7, Condition::Together, &[0]);
        probing.hunt = Some(HuntPlace {
            hunt: 1,
            group: 2,
            part: None,
            parts: 0,
            cut: 0,
            probe: Some(Member {
                core: core(2),
                offset: off(-12),
            }),
        });
        fx.trial = Some(probing);
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(said.label, "HUNT 1");
        assert_eq!(
            said.lines[0],
            "00 02 crashed together at their offsets, and every smaller part of them that was tested passed."
        );
        assert!(said.lines[1].ends_with("Now core 02."));
        assert_eq!(said.brief, "Probing members of 00 02: core 02 at -12.");
    }

    #[test]
    fn a_probe_says_failed_unless_its_group_crashed() {
        for (signal, verb) in [
            (Signal::Crash, "crashed"),
            (Signal::ComputationError, "failed"),
        ] {
            let mut fx = hunting(false, Vec::new());
            let (plan, facts) = fx.hunt.as_mut().expect("a hunt");
            plan.groups.push(HuntGroup {
                number: 1,
                cores: set(&[0, 2]),
                profile: PerCore::default(),
                stage: togi_journal::HuntStage::Part,
                probe: None,
                held: Vec::new(),
                outcome: GroupState::Failed,
                passed: 0,
                needed: 5,
                carried: false,
                running: false,
            });
            facts.group_signals.push((1, signal));
            facts.cause.signal = Some(Signal::Crash);
            let mut probing = trial(Regime::R7, Condition::Together, &[0]);
            probing.hunt = Some(HuntPlace {
                hunt: 1,
                group: 2,
                part: None,
                parts: 0,
                cut: 0,
                probe: Some(Member {
                    core: core(2),
                    offset: off(-12),
                }),
            });
            fx.trial = Some(probing);
            let said = story(&fx.scene(ts(1010)));
            assert!(
                said.lines[0].starts_with(&format!("00 02 {verb} together at their offsets")),
                "{}",
                said.lines[0]
            );
        }
    }

    fn probe_of(
        core_id: u8,
        offset: i64,
        failed_at: &[i64],
        passed_at: &[i64],
        done: bool,
    ) -> togi_tuner::MemberProbe {
        togi_tuner::MemberProbe {
            member: core(core_id),
            offset: off(offset),
            failed_at: failed_at.iter().map(|&at| off(at)).collect(),
            passed_at: passed_at.iter().map(|&at| off(at)).collect(),
            carried: Vec::new(),
            running: !done,
            done,
        }
    }

    fn probing_story(
        failed: &[u8],
        probes: Vec<togi_tuner::MemberProbe>,
        anchor_group: Option<GroupState>,
        at: i64,
        parked: i64,
    ) -> super::super::Story {
        let mut fx = hunting(true, Vec::new());
        let (plan, facts) = fx.hunt.as_mut().expect("a hunt");
        if parked != 0 {
            facts.parking = Some(crate::model::Parking {
                shallow: vec![(core(2), off(parked))],
                ..crate::model::Parking::default()
            });
        }
        plan.groups.push(HuntGroup {
            number: 1,
            cores: set(failed),
            profile: PerCore::default(),
            stage: togi_journal::HuntStage::Part,
            probe: None,
            held: Vec::new(),
            outcome: GroupState::Failed,
            passed: 0,
            needed: 5,
            carried: false,
            running: false,
        });
        if let Some(outcome) = anchor_group {
            plan.groups.push(HuntGroup {
                number: 2,
                cores: set(failed),
                profile: PerCore::default(),
                stage: togi_journal::HuntStage::Probe,
                probe: Some(Member {
                    core: core(2),
                    offset: off(parked),
                }),
                held: Vec::new(),
                outcome,
                passed: 0,
                needed: 5,
                carried: false,
                running: outcome == GroupState::Running,
            });
        }
        plan.probes = probes;
        facts.group_signals.push((1, Signal::Crash));
        let mut probing = trial(Regime::R7, Condition::Together, &[0]);
        probing.hunt = Some(HuntPlace {
            hunt: 1,
            group: 2,
            part: None,
            parts: 0,
            cut: 0,
            probe: Some(Member {
                core: core(2),
                offset: off(at),
            }),
        });
        fx.trial = Some(probing);
        story(&fx.scene(ts(1010)))
    }

    #[test]
    fn a_singleton_anchor_probe_claims_neither_a_combination_nor_a_pass() {
        let said = probing_story(
            &[2],
            vec![probe_of(2, 0, &[-12], &[], false)],
            Some(GroupState::Running),
            0,
            0,
        );
        assert_eq!(said.label, "HUNT 1");
        assert_eq!(
            said.lines[0],
            "Core 02 crashed on its own at -12, so it is the culprit."
        );
        assert!(
            said.lines[1].contains("No pass is known at its parked offset yet"),
            "{}",
            said.lines[1]
        );
        assert_eq!(
            said.brief,
            "Testing core 02 at 0, its parked offset, for a safe anchor."
        );
        for line in &said.lines {
            for wrong in ["together", "smaller part", "combination", "each member"] {
                assert!(!line.contains(wrong), "{line}");
            }
        }
    }

    #[test]
    fn a_singleton_parked_at_a_nonzero_offset_tests_that_offset_as_its_safe_anchor() {
        let said = probing_story(
            &[2],
            vec![probe_of(2, -10, &[-30], &[], false)],
            Some(GroupState::Running),
            -10,
            -10,
        );
        assert_eq!(
            said.lines[0],
            "Core 02 crashed on its own at -30, so it is the culprit."
        );
        assert!(
            said.lines[1].contains("No pass is known at its parked offset yet")
                && said.lines[1].contains("so I test core 02 at -10 to find a safe anchor"),
            "{}",
            said.lines[1]
        );
        assert_eq!(
            said.brief,
            "Testing core 02 at -10, its parked offset, for a safe anchor."
        );
        for line in said.lines.iter().chain([&said.brief]) {
            for wrong in [
                "A passing anchor is known",
                "between its failure",
                "together",
                "smaller part",
                "combination",
                "each member",
            ] {
                assert!(!line.contains(wrong), "{line}");
            }
        }
    }

    #[test]
    fn a_singleton_interior_probe_reads_as_what_it_is() {
        let interior = probing_story(
            &[2],
            vec![probe_of(2, -6, &[-12], &[], false)],
            Some(GroupState::Running),
            -6,
            0,
        );
        assert!(
            interior.lines[1].starts_with("A passing anchor is known"),
            "{}",
            interior.lines[1]
        );
        assert_eq!(
            interior.brief,
            "Probing core 02 at -6 between its failure and its passing anchor."
        );
    }

    #[test]
    fn an_ordinary_combination_probe_keeps_its_combination_story() {
        let said = probing_story(
            &[0, 2],
            vec![
                probe_of(0, -10, &[-12], &[], false),
                probe_of(2, -12, &[-12], &[], false),
            ],
            None,
            -12,
            0,
        );
        assert_eq!(
            said.lines[0],
            "00 02 crashed together at their offsets, and every smaller part of them that was tested passed."
        );
        assert!(said.lines[1].ends_with("Now core 02."));
        assert_eq!(said.brief, "Probing members of 00 02: core 02 at -12.");
    }

    #[test]
    fn a_combination_probe_of_its_first_member_says_that_member_goes_first() {
        let said = probing_story(
            &[2, 3],
            vec![
                probe_of(2, -12, &[-12], &[], false),
                probe_of(3, -10, &[-12], &[], false),
            ],
            None,
            -12,
            0,
        );
        assert!(
            said.lines[1].ends_with("Core 02 first."),
            "{}",
            said.lines[1]
        );
    }

    #[test]
    fn a_locating_hunt_reruns_its_load_with_the_candidates_at_zero() {
        let mut fx = hunting(false, Vec::new());
        fx.hunt.as_mut().expect("a hunt").0.groups.push(HuntGroup {
            number: 1,
            cores: set(&[0, 2]),
            profile: PerCore::default(),
            stage: togi_journal::HuntStage::Locate,
            probe: None,
            held: Vec::new(),
            outcome: GroupState::Running,
            passed: 0,
            needed: 5,
            carried: false,
            running: true,
        });
        let mut locating = trial(Regime::R7, Condition::Together, &[1]);
        locating.hunt = Some(HuntPlace {
            hunt: 1,
            group: 1,
            part: None,
            parts: 0,
            cut: 0,
            probe: None,
        });
        fx.trial = Some(locating);
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(said.label, "HUNT 1");
        assert_eq!(
            said.lines[0],
            "With no trial running and the offsets applied, the machine reported a wrong result."
        );
        assert_eq!(
            said.lines[1],
            "No core was named: I rerun that load with 00 02 at 0 and the loaded cores at their offsets: a pass puts the failure on the idle cores."
        );
        assert_eq!(
            said.brief,
            "Wrong result while idle; no core named. Rerunning it with 00 02 at 0."
        );
    }

    #[test]
    fn a_hunt_after_a_recovery_speaks_of_the_hunt_not_the_recovery() {
        let mut fx = hunting(true, Vec::new());
        fx.recover = Some(RecoveryView {
            crash_at: ts(980),
            reset: None,
            trial: None,
            end: None,
        });
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(said.label, "HUNT 1");
        assert!(said.lines[0].starts_with("I look for the cause among 00 02"));
        assert!(said.lines[1].ends_with("the rest parked at 0."));
        fx.hunt.as_mut().expect("a hunt").1.started = Some(ts(970));
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(said.label, "RECOVERED");
        fx.recover.as_mut().expect("recovery").end = Some({
            let mut end = end_view(Outcome::Failure, Some(Signal::Crash));
            end.core = Some(core(4));
            end
        });
        let said = story(&fx.scene(ts(1010)));
        assert_eq!(
            said.lines[1],
            "Core 04 was named: its failure point is recorded and it backs off."
        );
        assert_eq!(said.brief, said.lines[0]);
    }

    #[test]
    fn the_stage_label_names_the_stage_with_no_trial_to_speak_of() {
        let mut fx = Fx::new();
        assert_eq!(story(&fx.scene(ts(1))).label, "TOGI");
        fx.cycle = Some((cycle_plan(3, Vec::new(), 0), Vec::new(), None));
        assert_eq!(story(&fx.scene(ts(1))).label, "CYCLE 3");
        fx.session.as_mut().expect("a session").activity = Some(Activity::Deepening);
        assert_eq!(story(&fx.scene(ts(1))).label, "PHASE 2");
        let fx = hunting(false, Vec::new());
        assert_eq!(story(&fx.scene(ts(1))).label, "HUNT 1");
    }
}

mod the_narrator {
    use super::*;

    #[test]
    fn the_narrator_wraps_under_its_text() {
        let long = "word ".repeat(15);
        let said = super::super::Story {
            label: "HUNT 6".to_owned(),
            lines: vec![long],
            brief: String::new(),
            paint: Paint::Running,
        };
        let lines = text(&narrator_lines(&said, 60, 2, false, Colours::None));
        assert_eq!(lines.len(), 2, "{lines:?}");
        let first = lines[0].find("word").expect("text on the first row");
        assert_eq!(
            first,
            lines[1].find("word").expect("text on the second row")
        );
        assert!(first > lines[0].find("HUNT 6").expect("the label"));
        assert!(lines[0].starts_with("▌  HUNT 6   "));
        assert!(
            lines[1].starts_with(&format!("▌{}word", " ".repeat(11))),
            "{:?}",
            lines[1]
        );
    }

    #[test]
    fn a_story_too_tall_for_its_rows_falls_back_to_its_brief_then_to_cut_lines() {
        let said = super::super::Story {
            label: "CYCLE 1".to_owned(),
            lines: vec![
                "one two three four five six seven eight nine ten".to_owned(),
                "eleven twelve thirteen fourteen fifteen".to_owned(),
            ],
            brief: "a short line".to_owned(),
            paint: Paint::Running,
        };
        let rows_of_text = |height| text(&narrator_lines(&said, 30, height, false, Colours::None));
        let full = rows_of_text(10);
        assert!(full.len() > 2);
        assert_eq!(rows_of_text(1).len(), 1);
        assert!(rows_of_text(1)[0].contains("a short line"));
        let mut cut = said.clone();
        cut.brief.clear();
        let cut_rows = text(&narrator_lines(&cut, 30, 2, false, Colours::None));
        assert_eq!(cut_rows.len(), 2);
        assert!(
            cut_rows.iter().all(|row| row.chars().count() <= 30),
            "{cut_rows:?}"
        );
        assert!(cut_rows[0].contains("...") && cut_rows[1].contains("..."));
    }

    #[test]
    fn a_compact_screen_shows_the_brief_or_the_first_line_without_a_label() {
        let said = super::super::Story {
            label: "RERUN".to_owned(),
            lines: vec!["first".to_owned(), "second".to_owned()],
            brief: "the brief".to_owned(),
            paint: Paint::Running,
        };
        assert_eq!(
            text(&narrator_lines(&said, 40, 1, true, Colours::None)),
            ["▌  the brief"]
        );
        let mut plain_story = said.clone();
        plain_story.brief.clear();
        assert_eq!(
            text(&narrator_lines(&plain_story, 40, 1, true, Colours::None)),
            ["▌  first"]
        );
        let cut = text(&narrator_lines(&said, 10, 1, true, Colours::None));
        assert!(
            cut[0].starts_with("▌  ") && cut[0].chars().count() <= 10,
            "{cut:?}"
        );
    }

    #[test]
    fn the_bar_and_the_label_take_the_stories_paint() {
        let said = super::super::Story {
            label: "DEAD END".to_owned(),
            lines: vec!["x".to_owned()],
            brief: String::new(),
            paint: Paint::Bad,
        };
        let lines = narrator_lines(&said, 40, 1, false, Colours::Full);
        let bad = Paint::Bad.style(Colours::Full);
        assert_eq!(lines[0].spans[0].style, bad);
        assert_eq!(lines[0].spans[2].style, bad);
        assert_eq!(lines[0].spans[3].style, Paint::Text.style(Colours::Full));
        let none = narrator_lines(&said, 40, 1, false, Colours::None);
        assert!(
            none[0]
                .spans
                .iter()
                .all(|span| span.style == ratatui::style::Style::new())
        );
    }

    #[test]
    fn a_narrated_frame_stays_inside_its_bounds() {
        let said = super::super::Story {
            label: "NO SESSION YET".to_owned(),
            lines: vec!["Nothing has been recorded yet. ".repeat(6)],
            brief: String::new(),
            paint: Paint::Running,
        };
        for (w, h) in [(80, 24), (120, 33), (20, 8)] {
            let sc = screen(View::Main, w, h, Colours::None, true);
            let (buf, _) = draw_checked(&sc, |sc, buf| {
                let layout = Layout::new(sc);
                rows(
                    buf,
                    layout.body,
                    narrator_lines(
                        &said,
                        usize::from(layout.body.width),
                        usize::from(layout.body.height),
                        false,
                        sc.colours,
                    ),
                    sc.colours,
                );
                crate::screen::Drawn {
                    scroll: 0,
                    hold: crate::screen::Hold::Never,
                }
            });
            assert_frame_bounds(&buf, &sc);
            assert!(rows_of(&buf).iter().any(|row| row.contains('▌')), "{w}x{h}");
        }
    }
}

mod the_trial_band {
    use super::*;

    #[test]
    fn where_a_trial_loads_follows_its_kind() {
        let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0), (2, 1), (3, 1)]);
        let solo = alone(Regime::R1, 3, -16);
        assert_eq!(
            trial_where(&fx.scene(ts(1)), &solo),
            "on core 03 alone at -16"
        );
        let together = trial(Regime::R2, Condition::Together, &[0, 1, 2, 3]);
        assert_eq!(trial_where(&fx.scene(ts(1)), &together), "on 00-03");
        let full = in_cycle(
            trial(Regime::R7, Condition::Together, &[0, 1, 2, 3]),
            1,
            1,
            1,
            2,
        );
        assert_eq!(trial_where(&fx.scene(ts(1)), &full), "on 00-03, every CCD");
        let part = in_cycle(trial(Regime::R7, Condition::Together, &[0, 1]), 1, 1, 1, 2);
        assert_eq!(trial_where(&fx.scene(ts(1)), &part), "on 00 01");
        fx.cores.clear();
        assert_eq!(trial_where(&fx.scene(ts(1)), &full), "on 00-03");
    }

    #[test]
    fn a_hunt_trial_says_which_cores_it_parks_and_which_it_moves() {
        let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0), (2, 1)]);
        fx.shapes.insert(
            1,
            HuntShape {
                failing: crate::tests::kit::offsets(&[-10, -20, -30]),
                parked: crate::tests::kit::offsets(&[0, 0, 0]),
            },
        );
        let mut hunting = trial(Regime::R7, Condition::Together, &[0, 1]);
        hunting.hunt = Some(HuntPlace {
            hunt: 1,
            group: 1,
            part: Some(1),
            parts: 2,
            cut: 1,
            probe: None,
        });
        hunting.profile = crate::tests::kit::offsets(&[-10, -20, 0]);
        assert_eq!(
            trial_where(&fx.scene(ts(1)), &hunting),
            "on 00 01 · 02 parked at 0, idle"
        );
        hunting.profile = crate::tests::kit::offsets(&[-10, 0, 0]);
        assert_eq!(
            trial_where(&fx.scene(ts(1)), &hunting),
            "on 00 01 · 00 at failing offsets, 01 02 parked at 0"
        );
        hunting.profile = crate::tests::kit::offsets(&[-10, -20, -12]);
        assert_eq!(
            trial_where(&fx.scene(ts(1)), &hunting),
            "on 00 01 · core 02 at -12"
        );
        hunting.profile = crate::tests::kit::offsets(&[-10, -20, -30]);
        assert_eq!(
            trial_where(&fx.scene(ts(1)), &hunting),
            "on 00 01 · 00-02 at failing offsets"
        );
    }

    #[test]
    fn a_core_the_split_does_not_judge_is_in_neither_list() {
        let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0), (2, 1)]);
        fx.shapes.insert(
            1,
            HuntShape {
                failing: crate::tests::kit::offsets(&[-10, -20, -30]),
                parked: crate::tests::kit::offsets(&[0, -20, 0]),
            },
        );
        let mut hunting = trial(Regime::R7, Condition::Together, &[0, 1]);
        hunting.hunt = Some(HuntPlace {
            hunt: 1,
            group: 1,
            part: Some(1),
            parts: 2,
            cut: 1,
            probe: None,
        });
        hunting.profile = crate::tests::kit::offsets(&[-10, -20, 0]);
        assert_eq!(
            trial_where(&fx.scene(ts(1)), &hunting),
            "on 00 01 · 00 at failing offsets, 02 parked at 0, idle"
        );
    }

    #[test]
    fn the_operation_names_the_cycle_step_and_part() {
        let fx = Fx::new();
        let scene = fx.scene(ts(1));
        let step = in_cycle(trial(Regime::R2, Condition::Together, &[0]), 2, 3, 1, 1);
        assert_eq!(plain(&operation(&scene, &step)), "CYCLE 2 · STEP 3");
        let parted = in_cycle(trial(Regime::R2, Condition::Together, &[0]), 2, 3, 4, 6);
        assert_eq!(
            plain(&operation(&scene, &parted)),
            "CYCLE 2 · STEP 3 · PART 4"
        );
        let line = operation(&scene, &parted);
        assert_eq!(line.spans.len(), 1);
        let coloured = Scene::new(fx.snap(), ts(1), &UTC, Colours::Full);
        assert_eq!(
            operation(&coloured, &parted).spans[0].style,
            Paint::Running.style(Colours::Full)
        );
        let mut hunting = trial(Regime::R7, Condition::Together, &[0]);
        hunting.hunt = Some(HuntPlace {
            hunt: 2,
            group: 1,
            part: None,
            parts: 0,
            cut: 0,
            probe: None,
        });
        assert_eq!(
            operation(&coloured, &hunting).spans[0].style,
            Paint::Hunt.style(Colours::Full)
        );
    }

    #[test]
    fn crash_evidence_names_what_falls_short_of_a_core() {
        assert_eq!(crash_evidence(None), "");
        let mut end = end_view(Outcome::Failure, Some(Signal::Crash));
        assert_eq!(crash_evidence(Some(&end)), "");
        end.last_sample = Some(secs(125));
        assert_eq!(
            crash_evidence(Some(&end)),
            "the last sample came 2:05 into the trial"
        );
        end.stalled = Some(core(4));
        assert_eq!(
            crash_evidence(Some(&end)),
            "core 04's worker had stalled by the last sample, 2:05 into the trial"
        );
        end.signal = Some(Signal::ComputationError);
        assert_eq!(crash_evidence(Some(&end)), "");
    }
}

mod the_resting_band {
    use super::*;

    #[test]
    fn between_trials_names_the_last_outcome_and_what_comes_next_not_a_countdown() {
        let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0)]);
        fx.last = Some(end_view(Outcome::Pass, None));
        let band = resting_band(&fx.scene(ts(2000)));
        assert_eq!(band.title, "BETWEEN TRIALS");
        assert_eq!(band.paint, Paint::Grey);
        assert_eq!(band.lines[0], "last: PASS after 10:00 · R1 light on 00 01");
        assert_eq!(band.lines[1], "next trial: deciding");
        assert!(band.lines.iter().all(|line| !line.contains("left")));
        fx.forecast.between = Some(Between {
            next: Some(plan(
                Regime::R7,
                many(&[0, 1]),
                Condition::Together,
                cycle_ctx(1, 2),
                300,
            )),
            ..Between::default()
        });
        let band = resting_band(&fx.scene(ts(2000)));
        assert_eq!(
            band.lines[1],
            format!(
                "next trial: step 2: R7 all-core with {}",
                crate::words::workload_display(workloads(Regime::R7)[0].id())
            )
        );
        fx.forecast.between = Some(Between {
            uncertainty: Some(Uncertainty::NeedsRanking),
            ..Between::default()
        });
        assert_eq!(
            resting_band(&fx.scene(ts(2000))).lines[1],
            "next trial: read the core ranking before deciding"
        );
        fx.last = None;
        let band = resting_band(&fx.scene(ts(2000)));
        assert_eq!(band.lines[0], "No trial has run yet");
        assert_eq!(band.lines.len(), 2);
    }

    #[test]
    fn the_last_trial_names_its_peak_tctl_and_median_voltage_request() {
        let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0)]);
        fx.last = Some(TrialEndView {
            tctl_max_c: Some(81),
            voltage_v: Some(1.104),
            ..end_view(Outcome::Pass, None)
        });
        assert_eq!(
            resting_band(&fx.scene(ts(2000))).lines[0],
            "last: PASS after 10:00 · R1 light on 00 01 · Tctl max 81°C · 1.10 V median"
        );
    }

    #[test]
    fn a_recovery_says_what_the_crash_ended_how_late_and_what_runs_next() {
        let mut fx = Fx::new();
        let mut end = end_view(Outcome::Failure, Some(Signal::Crash));
        end.last_sample = Some(secs(100));
        end.planned = Some(secs(600));
        end.stalled = Some(core(1));
        fx.recover = Some(RecoveryView {
            crash_at: ts(3600),
            reset: Some(ResetKind::PowerLoss),
            trial: Some(in_cycle(
                trial(Regime::R7, Condition::Together, &[0, 1]),
                1,
                2,
                1,
                1,
            )),
            end: Some(end),
        });
        let band = resting_band(&fx.scene(ts(4000)));
        assert_eq!(band.title, "RECOVERED FROM A CRASH");
        assert_eq!(band.paint, Paint::Hunt);
        assert_eq!(
            band.lines[0],
            "crash in R7 all-core on 00 01 · cycle 1 step 2 · no core named"
        );
        assert_eq!(
            band.lines[1],
            format!(
                "early in the trial · last sample 1:40 of 10:00 · core 01's worker had stalled by then · detected {} after a power loss reset",
                crate::frames::scene::wall_second(&UTC, ts(3600))
            )
        );
        assert_eq!(band.lines[2], "next trial: deciding");
        fx.recover
            .as_mut()
            .expect("recovery")
            .end
            .as_mut()
            .expect("an end")
            .core = Some(core(1));
        assert!(resting_band(&fx.scene(ts(4000))).lines[0].ends_with("· core 01 named"));
        fx.recover.as_mut().expect("recovery").trial = None;
        fx.recover.as_mut().expect("recovery").reset = Some(ResetKind::Unknown);
        let band = resting_band(&fx.scene(ts(4000)));
        assert!(band.lines[0].starts_with("crash with no trial in flight · detected "));
        assert!(!band.lines[0].contains("reset"));
    }
}

mod the_simulated_sessions {
    use togi_watchtest::Scenario;

    use super::*;

    fn scene_of(model: &Model, events_end: Timestamp) -> Scene<'_> {
        Scene::new(model.snapshot(), events_end, &UTC, Colours::None)
    }

    fn model_of(scenario: Scenario) -> (Model, Timestamp) {
        let events = scenario.events().expect("the scenario's journal");
        let at = events.last().expect("an event").time;
        (crate::model::project(&events), at)
    }

    #[test]
    fn a_crashed_trial_and_its_recovery_are_told_apart() {
        let (model, at) = model_of(Scenario::Crashed);
        let scene = scene_of(&model, at);
        assert_eq!(story(&scene).label, "CRASHED");
        assert_eq!(resting_band(&scene).title, "CRASHED, NOT YET RECOVERED");
        let (model, at) = model_of(Scenario::Recovering);
        let scene = scene_of(&model, at);
        let band = resting_band(&scene);
        assert_eq!(band.title, "RECOVERED FROM A CRASH");
        assert!(band.lines[0].starts_with("crash in "), "{:?}", band.lines);
        assert!(
            band.lines
                .last()
                .expect("a line")
                .starts_with("next trial:")
        );
    }

    #[test]
    fn between_trials_the_last_outcome_and_the_next_trial_are_named() {
        let (model, at) = model_of(Scenario::Between);
        let scene = scene_of(&model, at);
        let band = resting_band(&scene);
        assert_eq!(band.title, "BETWEEN TRIALS");
        assert!(
            band.lines[0].starts_with("last: PASS after "),
            "{:?}",
            band.lines
        );
        assert!(
            band.lines[1].starts_with("next trial: "),
            "{:?}",
            band.lines
        );
        assert!(band.lines.iter().all(|line| !line.contains("left")));
    }

    #[test]
    fn a_stopped_session_names_its_stop_and_never_calls_the_rows_unapplied() {
        let (model, at) = model_of(Scenario::Stopped);
        let scene = scene_of(&model, at);
        let band = resting_band(&scene);
        assert_eq!(band.title, "STOPPED");
        assert_eq!(story(&scene).label, "STOPPED");
        assert!(band.lines.iter().all(|line| !line.contains("not applied")));
    }

    #[test]
    fn a_dead_end_session_keeps_its_recorded_detail() {
        let (model, at) = model_of(Scenario::Deadend);
        let scene = scene_of(&model, at);
        let detail = model
            .snapshot()
            .dead_end
            .expect("a dead end")
            .detail
            .clone();
        let band = resting_band(&scene);
        assert_eq!(band.title, "DEAD END");
        assert!(band.lines[0].ends_with(&detail), "{:?}", band.lines);
        assert_eq!(story(&scene).label, "DEAD END");
    }

    #[test]
    fn a_running_checking_trial_has_a_story_and_an_operation() {
        let (model, at) = model_of(Scenario::Checking);
        let scene = scene_of(&model, at + jiff::SignedDuration::from_secs(5));
        let said = story(&scene);
        assert!(
            said.label.starts_with("CYCLE ") || said.label.starts_with("RERUN"),
            "{}",
            said.label
        );
        let snapshot = model.snapshot();
        let trial = snapshot.trial.expect("a running trial");
        assert!(!plain(&operation(&scene, trial)).is_empty());
        assert!(trial_where(&scene, trial).starts_with("on "));
    }

    #[test]
    fn a_hunt_session_names_its_cause_and_its_split() {
        let (model, at) = model_of(Scenario::Hunt);
        let scene = scene_of(&model, at + jiff::SignedDuration::from_secs(5));
        let said = story(&scene);
        assert!(said.label.starts_with("HUNT "), "{}", said.label);
        assert_eq!(said.paint, Paint::Hunt);
        assert!(
            said.lines.len() == 2 && said.lines[1].contains("I rerun that load"),
            "{:?}",
            said.lines
        );
        let trial = model.snapshot().trial.expect("a hunt trial");
        assert!(plain(&operation(&scene, trial)).starts_with("HUNT "));
    }

    #[test]
    fn an_idle_session_holds_still_while_its_trial_is_in_flight() {
        let (model, at) = model_of(Scenario::Idle);
        let first = scene_of(&model, at + jiff::SignedDuration::from_secs(1));
        assert!(first.hold.is_some());
        let said = story(&first);
        assert!(
            said.lines[1].starts_with("This screen holds still while the idle trial is in flight,"),
            "{:?}",
            said.lines
        );
        for later in [600, 7200] {
            let scene = scene_of(&model, at + jiff::SignedDuration::from_secs(later));
            assert_eq!(story(&scene), said);
        }
    }

    #[test]
    fn a_session_between_phases_names_a_solo_search_in_its_labels() {
        let (model, at) = model_of(Scenario::Search);
        let scene = scene_of(&model, at + jiff::SignedDuration::from_secs(5));
        let said = story(&scene);
        assert_eq!(said.label, "SOLO LIMITS");
        assert!(
            said.lines[0].contains("solo limit") || said.lines[0].contains("candidate"),
            "{:?}",
            said.lines
        );
    }
}
