//! Function-level tests of the narration. Whole-frame assertions belong to the integration of the frames.

use std::sync::LazyLock;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::{
    Activity, Budget, DeadEndCondition, NightId, Outcome, PlannedTask, Queue, ShutdownReason,
    TaskId, TaskStatus, Tested,
};
use togi_machine::{CcdId, Regime, ResetKind, Signal};
use togi_tuner::policy::{
    DayProjection, FirmwareValidity, NightProjection, Params, Projection, QualificationProgress,
    State,
};
use togi_tuner::unit::{UnitId, UnitProfile};

use super::{
    last_lines, narrator_lines, operation, problem_story, resting_band, starting_story, story,
    trial_where,
};
use crate::frames::colours::Paint;
use crate::frames::fit::{inert, plain};
use crate::frames::scene::Scene;
use crate::frames::testing::{assert_frame_bounds, draw_checked, rows_of, screen};
use crate::frames::{draw::rows, layout::Layout};
use crate::model::{
    DeadEndView, Model, RecoveryView, SessionView, Snapshot, StopView, TrialEndView, TrialView,
};
use crate::screen::{Colours, View};
use crate::tests::fixtures;
use crate::tests::kit::{core, off, set};

mod edges;
mod nights;

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
    policy: Projection,
    trial: Option<TrialView>,
    last: Option<TrialEndView>,
    recover: Option<RecoveryView>,
    stopped: Option<StopView>,
    dead_end: Option<DeadEndView>,
    day: togi_journal::DayQuestions,
    cores: Vec<crate::model::CoreView>,
    applied_steps: std::collections::BTreeSet<togi_journal::DecisionId>,
}

impl Fx {
    fn new() -> Self {
        Self {
            model: Model::new(),
            session: Some(SessionView {
                start: ts(0),
                activity: None,
            }),
            policy: State::new(Params::SPEC).project(),
            trial: None,
            last: None,
            recover: None,
            stopped: None,
            dead_end: None,
            day: togi_journal::DayQuestions::default(),
            cores: Vec::new(),
            applied_steps: std::collections::BTreeSet::new(),
        }
    }

    fn without_session() -> Self {
        Self {
            session: None,
            ..Self::new()
        }
    }

    fn snap(&self) -> Snapshot<'_> {
        let mut s = self.model.snapshot();
        s.session = self.session.as_ref();
        s.policy = &self.policy;
        s.trial = self.trial.as_ref();
        s.last = self.last.as_ref();
        s.recover = self.recover.as_ref();
        s.stopped = self.stopped.as_ref();
        s.dead_end = self.dead_end.as_ref();
        s.day = &self.day;
        s.cores = &self.cores;
        s.applied_steps = &self.applied_steps;
        s
    }

    fn scene(&self, now: Timestamp) -> Scene<'_> {
        Scene::new(self.snap(), now, &UTC, Colours::None)
    }
}

/// A started night trial of `regime` on `cores`, a minute and a half into its intent.
fn trial(regime: Regime, cores: &[u8]) -> TrialView {
    TrialView {
        cores: set(cores),
        activity: Some(Activity::Mapping),
        started: Some(ts(1000)),
        ..fixtures::trial_view(regime)
    }
}

/// A night mapping trial: CCD `ccd` alone at `offset`, task `mapping 1`.
fn mapping(ccd: u8, offset: i64) -> TrialView {
    TrialView {
        task: Some(task(Queue::Mapping, 1)),
        tested: Some(Tested::Isolated {
            ccd: ccd_id(ccd),
            offset: off(offset),
        }),
        ..trial(Regime::R1, &[0, 1])
    }
}

fn ccd_id(ccd: u8) -> CcdId {
    CcdId::try_from(ccd).expect("a CCD id")
}

fn task(queue: Queue, ordinal: u32) -> TaskId {
    TaskId { queue, ordinal }
}

fn planned(queue: Queue, ordinal: u32, status: TaskStatus) -> PlannedTask {
    PlannedTask {
        task: task(queue, ordinal),
        status,
    }
}

/// A night plan with `tasks`, nothing running.
fn night(tasks: Vec<PlannedTask>) -> NightProjection {
    NightProjection {
        night: NightId(crate::tests::kit::seq(5)),
        started: ts(0),
        closed_by: None,
        budgets: vec![Budget {
            queue: Queue::Mapping,
            share_pct: 10,
            consumed_s: 600,
        }],
        tasks,
        current: None,
        mapping: Vec::new(),
        qualification: None,
        reproductions: Vec::new(),
        unavailable: Vec::new(),
        filler: 0,
    }
}

fn profile(offsets: &[(u8, i64)]) -> UnitProfile {
    UnitProfile::try_from(
        offsets
            .iter()
            .map(|&(ccd, offset)| (UnitId::Ccd(ccd_id(ccd)), off(offset)))
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .expect("a CCD profile")
}

fn text(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| plain(line, Colours::None))
        .collect()
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
        assert!(!said(&first).contains("ended"));
        fx.last = Some(end_view(Outcome::Pass, None));
        let after = story(&fx.scene(ts(10)));
        assert!(said(&after).contains("Trial t0 ended: pass."));
        assert!(!said(&after).contains("No trial has run yet"));
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
        assert!(drawn.contains(inert("\u{1b}[31m", Colours::None).as_ref()));
    }

    #[test]
    fn a_trial_intent_does_not_claim_the_offsets_are_set() {
        let mut fx = Fx::new();
        let mut waiting = trial(Regime::R6, &[0, 1]);
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
        let mut crashed = trial(Regime::R1, &[0]);
        crashed.crashed = true;
        crashed.id = "trial-B".to_owned().into();
        fx.trial = Some(crashed);
        let said = story(&fx.scene(ts(5000)));
        assert_eq!(said.label, "CRASHED");
        assert_eq!(said.paint, Paint::Warn);
        assert!(said.lines[0].contains("Trial trial-B crashed"));
        assert!(said.lines[0].contains("togi run records the crash"));
        for bad in ["Mapping", "past its planned end", "left"] {
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
        let mut crashed = trial(Regime::R1, &[0]);
        crashed.crashed = true;
        crashed.id = "trial-B".to_owned().into();
        crashed.started = None;
        fx.trial = Some(crashed);
        fx.recover = Some(RecoveryView {
            crash_at: ts(50),
            reset: None,
            trial: Some(trial(Regime::R1, &[0])),
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
        let mut crashed = trial(Regime::R1, &[0]);
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
        fx.trial = Some(trial(Regime::R1, &[0]));
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

    #[test_case::test_case(1000 + 899; "before end")]
    #[test_case::test_case(1000 + 900 + 42; "past end")]
    #[test_case::test_case(1000 + 7200; "long past end")]
    fn an_idle_trial_holds_the_story_until_its_end_is_recorded(at: i64) {
        let mut fx = Fx::new();
        let mut idle = trial(Regime::R6, &[0, 1]);
        idle.duration = secs(900);
        fx.trial = Some(idle);
        let first = story(&fx.scene(ts(1000 + 1)));
        assert_eq!(first.label, "MAPPING");
        assert!(first.lines[0].starts_with("Every core idles for 15 minutes"));
        assert!(
            first.lines[1].contains("holds still while the idle trial is in flight"),
            "{:?}",
            first.lines
        );
        assert_eq!(
            story(&fx.scene(ts(at))),
            first,
            "the idle story never moves with the clock"
        );
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
            "I stopped at a run limit an earlier version supported and restored baseline or shallower offsets"
        ));
        let band = resting_band(&fx.scene(ts(200)));
        assert_eq!(band.title, "STOPPED");
        assert_eq!(
            band.lines,
            [
                format!(
                    "{} · stopped at a run limit an earlier version supported",
                    crate::frames::scene::wall_second(&UTC, ts(100))
                ),
                "Rows show the saved profile, not applied now.".to_owned()
            ]
        );
    }

    #[test_case::test_case(ShutdownReason::FirstResult, false, "I stopped at a run limit an earlier version supported. Everything"; "first result")]
    #[test_case::test_case(ShutdownReason::Cycles, false, "I stopped at a run limit an earlier version supported. Everything"; "cycles")]
    #[test_case::test_case(ShutdownReason::Signal, true, "I restored baseline or shallower offsets before stopping"; "signal saved")]
    #[test_case::test_case(ShutdownReason::Signal, false, "I've stopped. Everything I learned is in the journal"; "signal unsaved")]
    fn a_stopped_session_preserves_its_reason(reason: ShutdownReason, saved: bool, want: &str) {
        let mut fx = Fx::new();
        fx.stopped = Some(StopView {
            at: ts(100),
            reason,
            saved,
        });
        assert!(said_of(&fx).contains(want), "{reason:?} {saved}");
    }

    fn said_of(fx: &Fx) -> String {
        story(&fx.scene(ts(200))).lines.join(" ")
    }

    #[test_case::test_case(DeadEndCondition::NoEvidence, "kernel log unreadable: permission denied", "installed"; "no evidence")]
    #[test_case::test_case(DeadEndCondition::Containment, "termination unconfirmed", "termination confirmed"; "containment")]
    #[test_case::test_case(DeadEndCondition::ThermalTrip, "firmware reported a thermal trip", "wrong result"; "thermal trip")]
    #[test_case::test_case(DeadEndCondition::FailureAtZero, "a control failed at CO 0", "installed"; "failure at zero")]
    fn a_dead_end_preserves_the_recorded_cause(
        condition: DeadEndCondition,
        detail: &str,
        wrong: &str,
    ) {
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

    #[test]
    fn a_dead_end_shows_the_saved_profile() {
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
        assert_eq!(
            resting_band(&fx.scene(ts(3)))
                .lines
                .last()
                .map(String::as_str),
            Some("Rows show the saved profile, not applied now.")
        );
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
    fn the_stage_label_names_the_stage_with_no_trial_to_speak_of() {
        let mut fx = Fx::new();
        assert_eq!(story(&fx.scene(ts(1))).label, "TOGI");
        fx.policy.applied = Some(profile(&[(0, -5)]));
        assert_eq!(story(&fx.scene(ts(1))).label, "DAY");
        fx.policy.night = Some(night(vec![planned(Queue::Mapping, 1, TaskStatus::Pending)]));
        assert_eq!(story(&fx.scene(ts(1))).label, "NIGHT");
    }
}

mod the_narrator {
    use super::*;

    #[test]
    fn the_narrator_wraps_under_its_text() {
        let long = "word ".repeat(15);
        let said = super::super::Story {
            label: "NIGHT".to_owned(),
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
        assert!(first > lines[0].find("NIGHT").expect("the label"));
        assert!(lines[0].starts_with("▌  NIGHT   "));
        assert!(
            lines[1].starts_with(&format!("▌{}word", " ".repeat(10))),
            "{:?}",
            lines[1]
        );
    }

    #[test]
    fn a_story_too_tall_for_its_rows_falls_back_to_its_brief_then_to_cut_lines() {
        let said = super::super::Story {
            label: "MAPPING".to_owned(),
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
    fn the_narrator_on_the_rich_profile_keeps_unicode_and_cuts_to_one_cell() {
        let said = super::super::Story {
            label: "RERUN".to_owned(),
            lines: vec!["日本語 – ab".to_owned()],
            brief: "日本語 – ab cd ef".to_owned(),
            paint: Paint::Running,
        };
        let rows = |lines: Vec<ratatui::text::Line<'static>>| -> Vec<String> {
            lines
                .iter()
                .map(|line| plain(line, Colours::Rich))
                .collect()
        };
        assert_eq!(
            rows(narrator_lines(&said, 17, 1, true, Colours::Rich)),
            ["▌  日本語 – ab…"]
        );
        assert_eq!(
            rows(narrator_lines(&said, 30, 1, false, Colours::Rich)),
            ["▌  RERUN   日本語 – ab"]
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

    #[test_case::test_case(80, 24; "small")]
    #[test_case::test_case(120, 33; "medium")]
    #[test_case::test_case(20, 8; "tiny")]
    fn a_narrated_frame_stays_inside_its_bounds(w: u16, h: u16) {
        let said = super::super::Story {
            label: "NO SESSION YET".to_owned(),
            lines: vec!["Nothing has been recorded yet. ".repeat(6)],
            brief: String::new(),
            paint: Paint::Running,
        };
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

mod the_trial_band {
    use super::*;

    #[test]
    fn where_a_trial_loads_names_its_cores() {
        let fx = Fx::new();
        let scene = fx.scene(ts(1));
        assert_eq!(
            trial_where(&scene, &trial(Regime::R7, &[0, 1, 2])),
            "on 00-02"
        );
    }

    #[test_case::test_case(Some(Activity::Mapping), Some(Tested::Isolated { ccd: ccd_id(1), offset: off(-15) }), false, "MAPPING CCD 1 AT -15"; "isolated mapping")]
    #[test_case::test_case(Some(Activity::Mapping), Some(Tested::Isolated { ccd: ccd_id(1), offset: off(-15) }), true, "MAPPING CCD 1 AT -15 · RETRY"; "retried mapping")]
    #[test_case::test_case(Some(Activity::Qualification), Some(Tested::Exact {}), false, "QUALIFYING -10/-5"; "exact qualification")]
    #[test_case::test_case(Some(Activity::Reproduction), Some(Tested::Exact {}), false, "REPRODUCING EXACT PROFILE"; "exact reproduction")]
    #[test_case::test_case(Some(Activity::Reproduction), Some(Tested::Isolated { ccd: ccd_id(0), offset: off(-20) }), false, "REPRODUCING CCD 0 AT -20"; "isolated reproduction")]
    #[test_case::test_case(Some(Activity::Control), Some(Tested::Exact {}), false, "CONTROLS AT CO 0"; "control")]
    fn the_operation_names_what_the_trial_is_for(
        activity: Option<Activity>,
        tested: Option<Tested>,
        retry: bool,
        want: &str,
    ) {
        let mut fx = Fx::new();
        let mut progress_night = night(Vec::new());
        progress_night.qualification = Some(QualificationProgress {
            profile: profile(&[(0, -10), (1, -5)]),
            outstanding: Vec::new(),
            witnessed: 0,
            eligible_s: 0,
        });
        fx.policy.night = Some(progress_night);
        let mut running = trial(Regime::R1, &[0, 1]);
        running.activity = activity;
        running.tested = tested;
        running.retry = retry;
        assert_eq!(
            plain(&operation(&fx.scene(ts(1001)), &running), Colours::None),
            want
        );
    }
}

mod the_resting_band {
    use super::*;
    use test_case::test_case;

    #[test]
    fn between_trials_names_the_last_outcome_and_what_comes_next_not_a_countdown() {
        let mut fx = Fx::new();
        fx.last = Some(end_view(Outcome::Pass, None));
        let band = resting_band(&fx.scene(ts(2000)));
        assert_eq!(band.title, "BETWEEN TRIALS");
        assert_eq!(band.paint, Paint::Grey);
        assert_eq!(band.lines[0], "last: PASS after 10:00 · R1 light on 00 01");
        assert_eq!(band.lines[1], "next trial: deciding");
        assert!(band.lines.iter().all(|line| !line.contains("left")));
        fx.policy.night = Some(night(vec![
            planned(Queue::Mapping, 3, TaskStatus::Pending),
            planned(Queue::Control, 4, TaskStatus::Pending),
        ]));
        assert_eq!(
            resting_band(&fx.scene(ts(2000))).lines[1],
            "next trial: mapping task 3"
        );
        fx.policy.night = Some(night(vec![planned(
            Queue::Mapping,
            3,
            TaskStatus::Complete,
        )]));
        assert_eq!(
            resting_band(&fx.scene(ts(2000))).lines[1],
            "next trial: filler"
        );
        fx.last = None;
        assert_eq!(resting_band(&fx.scene(ts(2000))).lines.len(), 1);
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
            trial: Some(trial(Regime::R7, &[0, 1])),
            end: Some(end),
        });
        let band = resting_band(&fx.scene(ts(4000)));
        assert_eq!(band.title, "RECOVERED FROM A CRASH");
        assert_eq!(band.paint, Paint::Warn);
        assert_eq!(band.lines[0], "crash in R7 all-core on 00 01");
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

    #[test_case::test_case(100, Some(600), "early in the trial · last sample 1:40 of 10:00"; "early")]
    #[test_case::test_case(300, Some(600), "midway through the trial · last sample 5:00 of 10:00"; "midway")]
    #[test_case::test_case(500, Some(600), "late in the trial · last sample 8:20 of 10:00"; "late")]
    #[test_case::test_case(100, None, "during the trial · last sample 1:40"; "no planned length")]
    #[test_case::test_case(100, Some(0), "during the trial · last sample 1:40"; "a zero planned length")]
    fn a_recovery_says_how_far_into_the_trial_the_crash_came(
        sample: u64,
        planned: Option<u64>,
        want: &str,
    ) {
        let mut fx = Fx::new();
        let mut end = end_view(Outcome::Failure, Some(Signal::Crash));
        end.last_sample = Some(secs(sample));
        end.planned = planned.map(secs);
        fx.recover = Some(RecoveryView {
            crash_at: ts(3600),
            reset: None,
            trial: Some(trial(Regime::R7, &[0, 1])),
            end: Some(end),
        });
        let band = resting_band(&fx.scene(ts(4000)));
        assert!(band.lines[1].starts_with(want), "{:?}", band.lines[1]);
    }

    #[test]
    fn the_last_trial_reads_as_a_word_and_its_measurements() {
        let mut fx = Fx::new();
        assert!(last_lines(&fx.scene(ts(1))).is_empty());
        let mut end = end_view(Outcome::Pass, None);
        end.tctl_max_c = Some(81);
        end.voltage_v = Some(1.1234);
        fx.last = Some(end);
        let lines = last_lines(&fx.scene(ts(1)));
        assert_eq!(
            text(&lines),
            ["last trial      PASS  10:00 on 00 01 · Tctl max 81°C · 1.12 V median"]
        );
        let full = Scene::new(fx.snap(), ts(1), &UTC, Colours::Full);
        assert_eq!(
            last_lines(&full)[0].spans[1].style,
            Paint::Good.style(Colours::Full)
        );
        fx.last = Some(end_view(Outcome::Failure, Some(Signal::ComputationError)));
        assert_eq!(
            text(&last_lines(&fx.scene(ts(1))))[0],
            "last trial      FAILURE  10:00 on 00 01 · wrong result"
        );
        fx.last = Some(end_view(Outcome::Failure, Some(Signal::Crash)));
        assert_eq!(
            text(&last_lines(&fx.scene(ts(1))))[0],
            "last trial      CRASH  on 00 01"
        );
    }

    #[test_case(Outcome::Failure, Some(Signal::ComputationError), "FAILURE", Paint::Bad; "a wrong result")]
    #[test_case(Outcome::Failure, Some(Signal::Crash), "CRASH", Paint::Bad; "a crash")]
    #[test_case(Outcome::Inconclusive, None, "INCONCLUSIVE", Paint::Warn; "an inconclusive trial")]
    fn the_last_trial_word_takes_its_outcomes_colour(
        outcome: Outcome,
        signal: Option<Signal>,
        word: &str,
        paint: Paint,
    ) {
        let mut fx = Fx::new();
        fx.last = Some(end_view(outcome, signal));
        let full = Scene::new(fx.snap(), ts(1), &UTC, Colours::Full);
        let line = &last_lines(&full)[0];
        assert_eq!(line.spans[1].content, word);
        assert_eq!(line.spans[1].style, paint.style(Colours::Full));
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
        assert_eq!(last_lines(&scene).len(), 1);
    }

    #[test]
    fn an_operator_ended_night_is_stopped_and_labels_saved_offsets_only_when_it_saved_them() {
        let (model, at) = model_of(Scenario::Stopped);
        let scene = scene_of(&model, at);
        let band = resting_band(&scene);
        assert_eq!(band.title, "STOPPED");
        assert_eq!(story(&scene).label, "STOPPED");
        let saved = model.snapshot().stopped.expect("a stop").saved;
        assert_eq!(
            band.lines
                .iter()
                .any(|line| line == "Rows show the saved profile, not applied now."),
            saved,
            "{:?}",
            band.lines
        );
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
}

mod the_day_question {
    use togi_journal::DayUncleanEnd;
    use togi_machine::{
        EndEvidence, GraphicsDriver, KernelFault, KernelTail, MceCount, PerCore, ResetKind,
        ResetReason,
    };

    use super::super::question_story;
    use crate::frames::colours::Paint;

    fn end() -> DayUncleanEnd {
        DayUncleanEnd {
            boot: "7a1b2c3d-0000-0000-0000-000000000000".to_owned().into(),
            last_checkpoint: None,
            offsets: PerCore::default(),
            reset_reason: ResetReason {
                kind: Some(ResetKind::Watchdog),
                raw: Vec::new(),
                supported: true,
            },
            evidence: EndEvidence {
                crashed_mces: MceCount::Read {
                    corrected: 0,
                    uncorrected: 0,
                },
                successor_mces: MceCount::Unknown {},
                tail: KernelTail::Faults {
                    faults: vec![KernelFault::GraphicsTimeout {
                        driver: GraphicsDriver::Amdgpu,
                    }],
                    logging_continued: true,
                },
                asleep: None,
            },
        }
    }

    #[test]
    fn says_the_reading_the_four_choices_and_where_to_answer() {
        let said = question_story(&end());
        assert_eq!(said.label, "DAY QUESTION");
        assert_eq!(said.paint, Paint::Warn);
        assert_eq!(
            said.lines,
            [
                format!("Boot 7a1b2c3d stopped unclean; reset reason: watchdog; reads as {}. What happened?", crate::day::class_words(end().class())),
                "1 it froze · 2 graphics died but sound went on · 3 I turned it off while it was working normally · 4 not sure. Answer: togi status".to_owned(),
            ]
        );
        assert_eq!(
            said.brief,
            "Boot 7a1b2c3d ended unclean: togi day answer 7a1b2c3d-0000-0000-0000-000000000000 frozen|graphics|power-off|unsure"
        );
    }
}

mod the_day_question_on_screen {
    use togi_journal::{
        DayAnswer, DayAnswerKind, DayQuestions, DayUncleanEnd, Event, Payload, Stamp,
    };
    use togi_machine::{
        EndEvidence, GraphicsDriver, KernelFault, KernelTail, MceCount, PerCore, ResetKind,
        ResetReason,
    };

    use super::{Fx, narrator_lines, plain, story, ts};
    use crate::frames::fit::fit_clauses;
    use crate::frames::footer::question_clause;
    use crate::screen::Colours;

    const BOOT: &str = "7a1b2c3d-0000-0000-0000-000000000000";

    fn events(answers: &[DayAnswerKind]) -> Vec<Event> {
        let end = DayUncleanEnd {
            boot: BOOT.to_owned().into(),
            last_checkpoint: None,
            offsets: PerCore::default(),
            reset_reason: ResetReason {
                kind: Some(ResetKind::Watchdog),
                raw: Vec::new(),
                supported: true,
            },
            evidence: EndEvidence {
                crashed_mces: MceCount::Read {
                    corrected: 0,
                    uncorrected: 0,
                },
                successor_mces: MceCount::Unknown {},
                tail: KernelTail::Faults {
                    faults: vec![KernelFault::GraphicsTimeout {
                        driver: GraphicsDriver::Amdgpu,
                    }],
                    logging_continued: true,
                },
                asleep: None,
            },
        };
        std::iter::once(Payload::from(end))
            .chain(answers.iter().map(|&answer| {
                Payload::from(DayAnswer {
                    boot: BOOT.to_owned().into(),
                    offsets: PerCore::default(),
                    answer,
                    uid: 1000,
                })
            }))
            .enumerate()
            .map(|(index, payload)| {
                Event::new(
                    togi_journal::Seq::new(u64::try_from(index).unwrap() + 1).unwrap(),
                    Stamp {
                        time: ts(0),
                        mono: None,
                    },
                    "next-boot".to_owned().into(),
                    Vec::new(),
                    payload,
                )
            })
            .collect()
    }

    fn fx(answers: &[DayAnswerKind]) -> Fx {
        let mut fx = Fx::new();
        fx.day = DayQuestions::of(&events(answers));
        fx
    }

    fn narration(fx: &Fx) -> String {
        let said = story(&fx.scene(ts(1000)));
        let lines = narrator_lines(&said, 156, 2, false, Colours::None);
        lines
            .iter()
            .map(|line| plain(line, Colours::None))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    #[test]
    fn a_pending_question_narrates_what_was_seen_and_how_to_answer() {
        assert_eq!(
            narration(&fx(&[])),
            include_str!("day-question-pending.txt")
        );
    }

    #[test]
    fn an_answer_ends_the_narration_and_a_changed_answer_does_too() {
        let none = narration(&Fx::new());
        assert_eq!(narration(&fx(&[DayAnswerKind::Frozen])), none);
        assert_eq!(
            narration(&fx(&[DayAnswerKind::Frozen, DayAnswerKind::Graphics])),
            none
        );
    }

    #[test]
    fn the_footer_counts_pending_questions_until_answered() {
        let footer = |fx: &Fx| {
            let pending = fx.snap().day.pending().count();
            (pending > 0).then(|| {
                plain(
                    &fit_clauses(
                        80,
                        &[question_clause(pending, Colours::None)],
                        Colours::None,
                    ),
                    Colours::None,
                )
            })
        };
        assert_eq!(
            footer(&fx(&[])).as_deref(),
            Some("1 day question pending: togi status")
        );
        assert_eq!(footer(&fx(&[DayAnswerKind::Unsure])), None);
    }
}
