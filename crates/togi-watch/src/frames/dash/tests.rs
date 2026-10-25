use std::time::Duration;

use jiff::tz::{TimeZone, offset};
use jiff::{SignedDuration, Timestamp};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use test_case::test_case;
use togi_journal::{
    Activity, DeadEndCondition, Event, Queue, Shutdown, ShutdownReason, Stamp, TaskId,
};
use togi_machine::{BootId, Regime};

use super::ccd::{ccd_role, coverage_line, draw_ccd, facts_line, gauge, marks, state_word};
use super::header::{header, status_header};
use super::now::{big_digits, filled, progress_bar};
use super::stage::stage_line;
use crate::frames::colours::Paint;
use crate::frames::fit::{line_width, plain};
use crate::frames::hand::{
    Hand, bare, cap, ccd, ccd_row, censored, core_row, fallback, mapping_trial, measured, night,
    observed, rate_bound, running_trial, with_cap,
};
use crate::frames::layout::SizeClass;
use crate::frames::measure::{Ccd, CoreColumns, measure};
use crate::frames::scene::Scene;
use crate::frames::testing::{FIXED_NOW, draw_checked, rows_of, screen};
use crate::model::{
    CoreView, DeadEndView, IdleHold, Model, RecoveryView, SessionView, Snapshot, StopView,
    TrialView,
};
use crate::screen::{Colours, View};
use crate::tests::fixtures::trial_view;
use crate::tests::kit::{config_loaded, seq};
use crate::tuning::TuningClock;

fn at(text: &str) -> Timestamp {
    text.parse().expect("a timestamp")
}

fn trial(regime: Regime, started: Option<Timestamp>, duration: u64) -> TrialView {
    TrialView {
        started,
        duration: Duration::from_secs(duration),
        ..trial_view(regime)
    }
}

fn session(start: Timestamp) -> SessionView {
    SessionView {
        start,
        activity: None,
    }
}

fn scene<'a>(s: &Snapshot<'a>, now: Timestamp, zone: &'a TimeZone, colours: Colours) -> Scene<'a> {
    Scene::new(*s, now, zone, colours)
}

fn header_text(s: &Snapshot<'_>, now: Timestamp, width: usize, compact: bool) -> String {
    plain(
        &header(
            &scene(s, now, &TimeZone::UTC, Colours::Full),
            width,
            compact,
        ),
        Colours::None,
    )
}

#[test_case::test_case(0, 0, "0 failures · 0 crashes"; "zero")]
#[test_case::test_case(1, 1, "1 failure · 1 crash"; "singular")]
#[test_case::test_case(2, 1, "2 failures · 1 crash"; "plural")]
fn header_counts_agree_in_number(failures: u32, crashes: u32, want: &str) {
    let model = Model::new();
    let start = at("2026-10-12T10:00:00Z");
    let now = at("2026-10-12T10:01:00Z");
    let view = session(start);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.failures = failures;
    s.crashes = crashes;
    assert!(header_text(&s, now, 240, false).contains(want), "{want}");
    let running = trial(Regime::R6, Some(at("2026-10-12T10:00:30Z")), 60);
    s.trial = Some(&running);
    let text = header_text(&s, now, 120, true);
    assert!(text.contains("paused · planned end"), "{text}");
    assert!(!text.contains("paused until"), "{text}");
    assert!(text.contains(want), "{text}");
}

#[test]
fn the_pause_notice_beside_the_clock_leaves_before_either_duration_is_shortened_or_cut() {
    let model = Model::new();
    let start = at("2026-10-12T10:00:00Z");
    let now = at("2026-10-12T10:01:00Z");
    let view = session(start);
    let running = trial(Regime::R6, Some(at("2026-10-12T10:00:30Z")), 60);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.trial = Some(&running);
    let roomy = header_text(&s, now, 120, true);
    assert!(roomy.contains("paused · planned end"), "{roomy}");
    let narrow = header_text(&s, now, 80, true);
    assert!(!narrow.contains("paused"), "{narrow}");
    assert!(
        narrow.contains("tuning 0h00m") && narrow.contains("session 0h00m"),
        "{narrow}"
    );
    assert!(narrow.contains("0 failures · 0 crashes"), "{narrow}");
    let tight = header_text(&s, now, 60, true);
    assert!(!tight.contains("paused"), "{tight}");
    assert!(
        tight.contains("tune 0h00m") && tight.contains("age 0h00m"),
        "{tight}"
    );
}

#[test]
fn the_header_drops_clauses_in_order_and_keeps_the_counts() {
    let model = Model::new();
    let start = at("2026-10-12T10:00:00Z");
    let view = session(start);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.failures = 3;
    s.crashes = 1;
    s.last_failure = Some(at("2026-10-12T10:20:00Z"));
    let now = at("2026-10-12T12:30:00Z");
    let full = header_text(&s, now, 240, false);
    assert!(full.contains("session 2h30m"), "{full}");
    assert!(
        full.contains("last observed failure 10:20, 2h10m ago"),
        "{full}"
    );
    let narrow = header_text(&s, now, 69, false);
    assert!(narrow.contains("3 failures · 1 crash"), "{narrow}");
    assert!(narrow.contains("tuning 0h00m"), "{narrow}");
    assert!(narrow.contains("session 2h30m"), "{narrow}");
    assert!(!narrow.contains("last observed"), "{narrow}");
    let tight = header_text(&s, now, 59, false);
    assert!(tight.contains("3 fail · 1 crash"), "{tight}");
    assert!(
        tight.contains("tune 0h00m") && tight.contains("age 2h30m"),
        "{tight}"
    );
    for width in 18..60 {
        let line = header(&scene(&s, now, &TimeZone::UTC, Colours::None), width, false);
        assert!(
            line_width(&line, Colours::None) <= width,
            "{width}: {}",
            plain(&line, Colours::None)
        );
    }
}

fn tuning_over(runs: &[(&str, Option<&str>)]) -> TuningClock {
    let event = |time: &str, payload| {
        Event::new(
            seq(1),
            Stamp {
                time: at(time),
                mono: None,
            },
            BootId::from("boot".to_owned()),
            Vec::new(),
            payload,
        )
    };
    let mut clock = TuningClock::default();
    for (start, end) in runs {
        clock.fold(&event(start, config_loaded()));
        if let Some(end) = end {
            let stop = Shutdown {
                reason: ShutdownReason::Signal,
                kernel_cursor: None,
                kernel_error: None,
                cycles: None,
            };
            clock.fold(&event(end, stop.into()));
        }
    }
    clock
}

#[test_case::test_case(160; "wide")]
#[test_case::test_case(80; "medium")]
fn the_header_shows_the_tuning_time_beside_the_session_age_and_freezes_both(width: usize) {
    let model = Model::new();
    let start = at("2026-10-12T10:00:00Z");
    let view = session(start);
    let clock = tuning_over(&[
        ("2026-10-12T10:00:00Z", Some("2026-10-12T11:00:00Z")),
        ("2026-10-12T12:00:00Z", None),
    ]);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.tuning = &clock;
    s.failures = 3;
    s.crashes = 1;
    s.last_failure = Some(at("2026-10-12T10:20:00Z"));
    let now = at("2026-10-12T12:20:00Z");
    let text = header_text(&s, now, width, false);
    let tuning = text.find("tuning 1h20m");
    let age = text.find("session 2h20m");
    assert!(tuning.is_some() && tuning < age, "{width}: {text}");
    let wide = header_text(&s, now, 160, false);
    assert!(
        wide.contains("last observed failure 10:20, 2h00m ago"),
        "{wide}"
    );
    for width in 18..=160 {
        let line = header(&scene(&s, now, &TimeZone::UTC, Colours::None), width, false);
        assert!(
            line_width(&line, Colours::None) <= width,
            "{width}: {}",
            plain(&line, Colours::None)
        );
    }

    let later = at("2026-10-12T14:00:00Z");
    let stop = StopView {
        at: at("2026-10-12T12:05:00Z"),
        reason: ShutdownReason::Signal,
        saved: true,
    };
    let mut stopped = s;
    stopped.stopped = Some(&stop);
    let text = header_text(&stopped, later, 160, false);
    assert!(
        text.contains("tuning 1h05m") && text.contains("session 2h05m"),
        "{text}"
    );

    let dead = DeadEndView {
        at: at("2026-10-12T12:10:00Z"),
        condition: DeadEndCondition::Smu,
        detail: String::new(),
    };
    let mut ended = s;
    ended.dead_end = Some(&dead);
    let text = header_text(&ended, later, 160, false);
    assert!(
        text.contains("tuning 1h10m") && text.contains("session 2h10m"),
        "{text}"
    );

    let running = trial(Regime::R6, Some(at("2026-10-12T12:15:00Z")), 900);
    let mut idle = s;
    idle.trial = Some(&running);
    let zone = TimeZone::UTC;
    let held = scene(&idle, later, &zone, Colours::None);
    let text = plain(&header(&held, 160, false), Colours::None);
    assert!(
        text.contains("tuning 1h15m") && text.contains("session 2h15m"),
        "{text}"
    );
}

#[test]
fn a_journal_that_is_not_folded_names_its_status_and_no_earlier_run() {
    let now = at("2026-10-12T10:00:00Z");
    let line = plain(
        &status_header(now, &TimeZone::UTC, Colours::Full, "starting", 80),
        Colours::None,
    );
    assert_eq!(line, "togi   10:00:00   starting");
    let missing = header_text(&Model::new().snapshot(), now, 80, false);
    assert_eq!(missing, "togi   10:00:00   no session yet");
    for stale in ["STOPPED", "no session yet", "NO SESSION YET"] {
        assert!(!line.contains(stale));
    }
}

#[test]
fn every_time_on_screen_is_local() {
    let east = TimeZone::fixed(offset(9));
    let model = Model::new();
    let start = at("2026-10-12T10:00:00Z");
    let view = session(start);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.last_failure = Some(at("2026-10-12T10:20:00Z"));
    let now = at("2026-10-12T10:30:45Z");
    let text = plain(
        &header(&scene(&s, now, &east, Colours::Full), 240, false),
        Colours::None,
    );
    assert!(text.contains("togi   19:30:45"), "{text}");
    assert!(text.contains("last observed failure 19:20"), "{text}");
    let line = plain(
        &status_header(now, &east, Colours::Full, "starting", 80),
        Colours::None,
    );
    assert!(line.starts_with("togi   19:30:45"), "{line}");

    let running = trial(Regime::R6, Some(start), 600);
    s.trial = Some(&running);
    let held = scene(&s, at("2026-10-12T11:00:00Z"), &east, Colours::Full);
    assert_eq!(held.now, start);
    let head = plain(&header(&held, 240, true), Colours::None);
    assert!(head.starts_with("togi   19:00:00"), "{head}");
    assert!(head.contains("paused · planned end 19:10"), "{head}");
    assert!(!head.contains("paused until"), "{head}");
    let stage = plain(&stage_line(&held, SizeClass::Wide, true), Colours::None);
    assert!(
        stage.contains("screen paused · planned end 19:10"),
        "{stage}"
    );
    assert!(!stage.contains("paused until"), "{stage}");
}

fn stage_of(s: &Snapshot<'_>, now: Timestamp, class: SizeClass) -> String {
    plain(
        &stage_line(&scene(s, now, &TimeZone::UTC, Colours::Full), class, false),
        Colours::None,
    )
}

/// A hand with a mapping trial of CCD 1 at -15 running inside a night that started at 22:13 UTC and has observed
/// half an hour.
fn night_hand() -> Hand {
    let mut hand = Hand::new(
        vec![
            core_row(0, 0, Some(0), false),
            core_row(8, 1, Some(0), true),
        ],
        bare(vec![ccd_row(0, &[0], 0), ccd_row(1, &[8], 0)]),
    );
    hand.policy.night = Some(night(Some((Queue::Mapping, 2, Activity::Mapping))));
    hand.trial = Some(mapping_trial(1, -15, &[8]));
    hand
}

#[test]
fn the_stage_line_names_the_night_its_start_its_elapsed_time_and_what_runs() {
    let hand = night_hand();
    let now = FIXED_NOW + SignedDuration::from_secs(40);
    let wide = stage_of(&hand.snapshot(), now, SizeClass::Wide);
    assert!(wide.starts_with("► NIGHT since 22:13 · 0h30m"), "{wide}");
    assert!(wide.contains("mapping CCD 1 at -15"), "{wide}");
    let compact = stage_of(&hand.snapshot(), now, SizeClass::Compact);
    assert!(compact.starts_with("► NIGHT 0h30m"), "{compact}");
    assert!(compact.contains("mapping C1 -15"), "{compact}");
    for old in ["SOLO", "PHASE", "CLEAN", "CYCLE", "HUNT", "until"] {
        assert!(
            !wide.contains(old) && !compact.contains(old),
            "{wide} {compact}"
        );
    }
}

#[test_case(true, "filler on CCD 1", "filler C1"; "a filler trial on one CCD")]
#[test_case(false, "filler", "filler"; "a filler task before its first trial")]
fn the_stage_line_names_the_filler_by_the_ccd_its_trial_tests(
    isolated: bool,
    wide_words: &str,
    compact_words: &str,
) {
    let mut hand = night_hand();
    let filler = TaskId {
        queue: Queue::Filler,
        ordinal: 7,
    };
    if let Some(trial) = hand.trial.as_mut() {
        trial.task = Some(filler);
        if !isolated {
            trial.tested = None;
        }
    }
    let now = FIXED_NOW + SignedDuration::from_secs(40);
    let wide = stage_of(&hand.snapshot(), now, SizeClass::Wide);
    assert!(wide.ends_with(wide_words), "{wide}");
    let compact = stage_of(&hand.snapshot(), now, SizeClass::Compact);
    assert!(compact.ends_with(compact_words), "{compact}");
}

#[test]
fn between_trials_the_stage_line_names_the_committed_task_not_the_last_trials_activity() {
    let mut hand = night_hand();
    hand.trial = None;
    hand.session.activity = Some(Activity::Mapping);
    if let Some(night) = hand.policy.night.as_mut() {
        night.current = Some((
            TaskId {
                queue: Queue::Control,
                ordinal: 3,
            },
            Some(Activity::Control),
        ));
    }
    let now = FIXED_NOW + SignedDuration::from_secs(40);
    let wide = stage_of(&hand.snapshot(), now, SizeClass::Wide);
    assert!(wide.ends_with("controls at CO 0"), "{wide}");
    assert!(!wide.contains("mapping"), "{wide}");
    if let Some(night) = hand.policy.night.as_mut() {
        night.current = None;
    }
    let wide = stage_of(&hand.snapshot(), now, SizeClass::Wide);
    assert!(!wide.contains("mapping"), "no task is current: {wide}");
}

#[test]
fn the_stage_line_names_the_day_once_a_later_boot_closes_the_night() {
    let mut hand = night_hand();
    hand.trial = Some(running_trial(Regime::R1, &[8]));
    let late = FIXED_NOW + SignedDuration::from_hours(7);
    let open = stage_of(&hand.snapshot(), late, SizeClass::Wide);
    assert!(
        open.starts_with("► NIGHT since 22:13"),
        "a night has no deadline: {open}"
    );
    if let Some(night) = hand.policy.night.as_mut() {
        night.closed_by = Some(crate::tests::kit::seq(2));
    }
    let wide = stage_of(&hand.snapshot(), late, SizeClass::Wide);
    assert!(wide.starts_with("► DAY"), "{wide}");
    assert!(!wide.contains("NIGHT"), "{wide}");
}

#[test]
fn a_stop_a_dead_end_and_a_recovery_suppress_the_running_marker() {
    let hand = night_hand();
    let now = FIXED_NOW + SignedDuration::from_secs(40);
    let stopped = StopView {
        at: FIXED_NOW,
        reason: ShutdownReason::Signal,
        saved: false,
    };
    let mut s = hand.snapshot();
    s.stopped = Some(&stopped);
    let text = stage_of(&s, now, SizeClass::Wide);
    assert!(text.starts_with("○ STOPPED"), "{text}");
    let dead = DeadEndView {
        at: FIXED_NOW,
        condition: DeadEndCondition::BootLoop,
        detail: String::new(),
    };
    let mut s = hand.snapshot();
    s.dead_end = Some(&dead);
    let text = stage_of(&s, now, SizeClass::Wide);
    assert!(text.starts_with("○ DEAD END"), "{text}");
    let recovery = RecoveryView {
        crash_at: FIXED_NOW,
        reset: None,
        trial: None,
        end: None,
    };
    let mut s = hand.snapshot();
    s.recover = Some(&recovery);
    let text = stage_of(&s, now, SizeClass::Wide);
    assert!(text.starts_with("○ NIGHT since 22:13"), "{text}");
    assert!(!text.contains('►'), "{text}");
    let none = stage_line(
        &scene(
            &Model::new().snapshot(),
            FIXED_NOW,
            &TimeZone::UTC,
            Colours::Full,
        ),
        SizeClass::Wide,
        false,
    );
    assert_eq!(line_width(&none, Colours::None), 0);
}

#[test]
fn a_summary_stage_line_ends_in_the_trials_time() {
    let model = Model::new();
    let start = at("2026-10-12T10:00:00Z");
    let view = session(start);
    let running = trial(Regime::R1, Some(start), 600);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.trial = Some(&running);
    let line = |now: &str, crashed: bool| {
        let mut t = running.clone();
        t.crashed = crashed;
        let mut s = s;
        s.trial = Some(&t);
        plain(
            &stage_line(
                &scene(&s, at(now), &TimeZone::UTC, Colours::Full),
                SizeClass::Medium,
                true,
            ),
            Colours::None,
        )
    };
    assert!(line("2026-10-12T10:04:00Z", false).ends_with("6:00 left"));
    assert!(line("2026-10-12T10:11:00Z", false).ends_with("1:00 past its end"));
    assert!(line("2026-10-12T10:04:00Z", true).ends_with("trial crashed"));
}

fn role_of_ccd_zero(loaded: &[bool], runs: bool) -> String {
    let cores: Vec<CoreView> = loaded
        .iter()
        .enumerate()
        .map(|(i, &loaded)| core_row(u8::try_from(i).unwrap(), 0, Some(0), loaded))
        .collect();
    let mut hand = Hand::new(cores, bare(vec![]));
    hand.trial = runs.then(|| running_trial(Regime::R1, &[0]));
    let zone = TimeZone::UTC;
    let snapshot = hand.snapshot();
    ccd_role(&scene(&snapshot, FIXED_NOW, &zone, Colours::None), ccd(0))
}

#[test_case(&[true, true], "all loaded"; "every core loaded")]
#[test_case(&[false, false], "idle"; "nothing loaded")]
#[test_case(&[true, false], "one loaded"; "one loaded")]
#[test_case(&[true, true, false], "2 loaded"; "two of three")]
#[test_case(&[], ""; "no cores")]
fn a_ccd_role_counts_what_the_running_trial_loads(loaded: &[bool], want: &str) {
    assert_eq!(role_of_ccd_zero(loaded, true), want);
}

#[test]
fn without_a_running_trial_no_core_is_loaded() {
    assert_eq!(role_of_ccd_zero(&[true], false), "idle");
    let hand = Hand::new(vec![core_row(0, 0, Some(0), true)], bare(vec![]));
    let zone = TimeZone::UTC;
    let snapshot = hand.snapshot();
    let scene = scene(&snapshot, FIXED_NOW, &zone, Colours::None);
    assert_eq!(state_word(&scene, &snapshot.cores[0]), "");
}

#[test_case(true, "LOADED"; "loaded")]
#[test_case(false, ""; "not loaded")]
fn a_running_trial_gives_each_core_its_word(loaded: bool, want: &str) {
    let mut hand = Hand::new(vec![core_row(0, 0, Some(0), loaded)], bare(vec![]));
    hand.trial = Some(running_trial(Regime::R1, &[0]));
    let zone = TimeZone::UTC;
    let snapshot = hand.snapshot();
    let scene = scene(&snapshot, FIXED_NOW, &zone, Colours::None);
    assert_eq!(state_word(&scene, &snapshot.cores[0]), want);
}

fn gauge_of(
    colours: Colours,
    loaded: bool,
    projection: &togi_tuner::policy::CcdProjection,
    applied: i64,
    cells: usize,
) -> Line<'static> {
    let hand = Hand::new(
        vec![core_row(0, 0, Some(applied), loaded)],
        bare(vec![projection.clone()]),
    );
    let zone = TimeZone::UTC;
    let snapshot = hand.snapshot();
    let scene = scene(&snapshot, FIXED_NOW, &zone, colours);
    gauge(
        &scene,
        &snapshot.cores[0],
        Some(projection),
        Some(applied),
        cells,
    )
}

fn gauge_text(
    colours: Colours,
    projection: &togi_tuner::policy::CcdProjection,
    applied: i64,
    cells: usize,
) -> String {
    plain(
        &gauge_of(colours, true, projection, applied, cells),
        colours,
    )
    .trim_end()
    .to_owned()
}

fn capped(edge: Option<togi_journal::Edge>) -> togi_tuner::policy::CcdProjection {
    with_cap(
        ccd_row(0, &[0], -10),
        cap(-26, rate_bound(), fallback(), 2, 0),
        edge,
        14,
    )
}

#[test]
fn a_gauge_shows_the_applied_depth_the_cap_track_and_the_edge() {
    let mark = marks(Colours::None).applied;
    let projection = capped(Some(observed(-31)));
    let want = format!(
        "{}{}    {mark}",
        mark.to_string().repeat(12),
        "·".repeat(14)
    );
    assert_eq!(gauge_text(Colours::None, &projection, -12, 50), want);
    let line = gauge_of(Colours::Full, true, &projection, -12, 50);
    let styles: Vec<_> = line.spans.iter().map(|span| span.style).collect();
    let blank = ratatui::style::Style::new();
    assert_eq!(
        styles,
        [
            Paint::White.style(Colours::Full),
            Paint::Grey.style(Colours::Full),
            blank,
            Paint::Bad.style(Colours::Full),
            blank,
        ],
        "equal runs share one span; the observed edge is red"
    );
}

#[test]
fn a_censored_edge_is_amber_and_an_unknown_one_is_not_drawn() {
    let censor = gauge_of(Colours::Full, true, &capped(Some(censored(-50))), -12, 50);
    let last = censor.spans.last().expect("a span");
    assert_eq!(last.style, Paint::Warn.style(Colours::Full));
    assert_eq!(last.content.trim_end().chars().count(), 1);
    let none = gauge_text(Colours::None, &capped(None), -12, 50);
    assert_eq!(none, format!("{}{}", "▄".repeat(12), "·".repeat(14)));
}

#[test]
fn an_unloaded_applied_depth_is_grey_not_white() {
    let line = gauge_of(Colours::Full, false, &capped(None), -12, 50);
    assert_eq!(line.spans[0].style, Paint::Grey.style(Colours::Full));
}

#[test]
fn a_gauge_in_twenty_five_cells_takes_two_counts_to_a_cell() {
    let projection = capped(Some(observed(-31)));
    let want = format!("{}{}  ▄", "▄".repeat(6), "·".repeat(7));
    assert_eq!(gauge_text(Colours::Full, &projection, -12, 25), want);
    let rich = gauge_text(Colours::Rich, &projection, -12, 25);
    assert_eq!(rich, want.replace('▄', "▆"));
}

#[test]
fn the_edge_cell_is_the_edge_even_where_the_applied_depth_passes_it() {
    let projection = capped(Some(observed(-31)));
    let want = format!("{}▄{}", "▄".repeat(30), "▄".repeat(9));
    assert_eq!(gauge_text(Colours::Full, &projection, -40, 50), want);
}

fn ccd_projection() -> togi_tuner::policy::CcdProjection {
    with_cap(
        ccd_row(0, &[0, 1], -10),
        cap(-15, rate_bound(), fallback(), 2, 1),
        Some(observed(-23)),
        5,
    )
}

fn facts_text(projection: &togi_tuner::policy::CcdProjection, width: usize) -> String {
    let hand = Hand::new(vec![], bare(vec![projection.clone()]));
    let zone = TimeZone::UTC;
    let snapshot = hand.snapshot();
    plain(
        &facts_line(
            &scene(&snapshot, FIXED_NOW, &zone, Colours::None),
            projection,
            width,
        ),
        Colours::None,
    )
}

fn coverage_text(projection: &togi_tuner::policy::CcdProjection, width: usize) -> String {
    let hand = Hand::new(vec![], bare(vec![projection.clone()]));
    let zone = TimeZone::UTC;
    let snapshot = hand.snapshot();
    plain(
        &coverage_line(
            &scene(&snapshot, FIXED_NOW, &zone, Colours::None),
            projection,
            width,
        ),
        Colours::None,
    )
}

#[test]
fn the_facts_row_names_day_cap_binding_edge_and_distance() {
    assert_eq!(
        facts_text(&ccd_projection(), 115),
        "D -10 · S -15 fallback slope · bound by rate R1 · U fails at -23 · 5 counts to S"
    );
    let measured_cap = with_cap(
        ccd_row(0, &[0, 1], -10),
        cap(-15, rate_bound(), measured(), 2, 0),
        Some(censored(-50)),
        5,
    );
    let text = facts_text(&measured_cap, 115);
    assert!(text.contains("measured slope"), "{text}");
    assert!(text.contains("U censored at -50"), "{text}");
}

#[test]
fn a_ccd_with_no_cap_or_edge_says_so() {
    let text = facts_text(&ccd_row(0, &[0], 0), 115);
    assert!(text.starts_with("D 0 · "), "{text}");
    assert!(text.contains("no cap"), "{text}");
    assert!(text.contains("U unknown"), "{text}");
    let covered = coverage_text(&ccd_row(0, &[0], 0), 115);
    assert!(covered.contains("no cap"), "{covered}");
}

#[test]
fn the_coverage_row_counts_what_the_cap_has_and_lacks() {
    assert_eq!(
        coverage_text(&ccd_projection(), 115),
        "2 covered for the cap · 1 uncovered for the cap"
    );
}

#[test]
fn the_facts_row_never_cuts_a_value_whatever_the_width() {
    let whole = [
        "D -10",
        "S -15 fallback slope",
        "S -15",
        "bound by rate R1",
        "rate R1",
        "U fails at -23",
        "U -23",
        "5 counts to S",
        "5 to S",
    ];
    for width in 21..=120 {
        let text = facts_text(&ccd_projection(), width);
        assert!(text.chars().count() <= width, "width {width}: {text:?}");
        for piece in text.split(" · ") {
            assert!(
                whole.contains(&piece),
                "width {width}: {piece:?} in {text:?}"
            );
        }
    }
}

const WIDE: CoreColumns = CoreColumns {
    id: 2,
    offset: 6,
    state: 11,
    gauge: 21,
    cells: 50,
};

fn table(hand: &Hand, width: u16, height: u16, colours: Colours) -> Vec<String> {
    let rect = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(rect);
    let zone = TimeZone::UTC;
    let snapshot = hand.snapshot();
    let scene = scene(&snapshot, FIXED_NOW, &zone, colours);
    draw_ccd(
        &mut buf,
        &Ccd {
            id: ccd(0),
            rect,
            columns: WIDE,
        },
        &scene,
    );
    rows_of(&buf)
}

fn two_cores() -> Hand {
    Hand::new(
        vec![
            core_row(0, 0, Some(-10), true),
            core_row(1, 0, Some(-4), false),
        ],
        bare(vec![ccd_projection()]),
    )
}

#[test]
fn a_ccd_table_has_a_heading_two_fact_rows_and_a_row_per_core() {
    let mut hand = two_cores();
    hand.trial = Some(running_trial(Regime::R1, &[0]));
    let rows = table(&hand, 115, 5, Colours::None);
    assert!(rows[0].starts_with("CCD 0 one loaded"), "{:?}", rows[0]);
    assert!(rows[0].contains("-50"), "{:?}", rows[0]);
    assert!(rows[1].starts_with("D -10 · S -15"), "{:?}", rows[1]);
    assert!(
        rows[2].starts_with("2 covered for the cap"),
        "{:?}",
        rows[2]
    );
    assert!(rows[3].starts_with("► 00"), "{:?}", rows[3]);
    assert!(
        rows[3].contains("-10") && rows[3].contains("LOAD"),
        "{:?}",
        rows[3]
    );
    assert!(rows[4].starts_with("  01"), "{:?}", rows[4]);
}

#[test]
fn a_core_with_no_readback_shows_a_question_mark_not_zero() {
    let hand = Hand::new(
        vec![core_row(0, 0, None, false)],
        bare(vec![ccd_projection()]),
    );
    let rows = table(&hand, 115, 4, Colours::None);
    assert!(rows[3][..WIDE.state].contains('?'), "{:?}", rows[3]);
}

#[test]
fn a_readback_is_never_clamped() {
    let hand = Hand::new(
        vec![core_row(0, 0, Some(-60), false)],
        bare(vec![ccd_projection()]),
    );
    let rows = table(&hand, 115, 4, Colours::None);
    assert!(rows[3].contains("-60"), "{:?}", rows[3]);
}

#[test]
fn a_restored_session_shows_the_saved_profile_in_its_rows() {
    let mut hand = Hand::new(
        vec![CoreView {
            profile: crate::tests::kit::off(-7),
            ..core_row(0, 0, None, false)
        }],
        bare(vec![ccd_projection()]),
    );
    let stop = StopView {
        at: FIXED_NOW,
        reason: ShutdownReason::Signal,
        saved: true,
    };
    let zone = TimeZone::UTC;
    let mut snapshot = hand.snapshot();
    snapshot.stopped = Some(&stop);
    let mut buf = Buffer::empty(Rect::new(0, 0, 115, 4));
    let scene = scene(&snapshot, FIXED_NOW, &zone, Colours::None);
    draw_ccd(
        &mut buf,
        &Ccd {
            id: ccd(0),
            rect: Rect::new(0, 0, 115, 4),
            columns: WIDE,
        },
        &scene,
    );
    assert!(rows_of(&buf)[3].contains("-7"), "{:?}", rows_of(&buf)[3]);
    hand.cores.clear();
}

#[test]
fn cores_that_do_not_fit_are_counted() {
    let cores: Vec<CoreView> = (0..5).map(|i| core_row(i, 0, Some(0), false)).collect();
    let hand = Hand::new(cores, bare(vec![ccd_projection()]));
    let rows = table(&hand, 115, 6, Colours::None);
    assert!(rows[3].starts_with("  00"), "{:?}", rows[3]);
    assert!(rows[4].starts_with("  01"), "{:?}", rows[4]);
    assert!(rows[5].starts_with("+3 more"), "{:?}", rows[5]);
}

#[test]
fn a_ccd_the_tuner_has_no_state_for_says_so() {
    let hand = Hand::new(vec![core_row(0, 0, Some(0), false)], bare(vec![]));
    let rows = table(&hand, 115, 4, Colours::None);
    assert!(
        rows[1].starts_with("no tuner state for this CCD yet"),
        "{:?}",
        rows[1]
    );
}

#[test]
fn the_trial_bar_shows_elapsed_without_colour() {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let scene = scene(
        &model.snapshot(),
        Timestamp::UNIX_EPOCH,
        &zone,
        Colours::None,
    );
    let total = Duration::from_secs(120);
    let bar = |elapsed: u64| {
        let line = progress_bar(&scene, 20, Duration::from_secs(elapsed), total);
        assert!(
            line.spans
                .iter()
                .all(|span| span.style == ratatui::style::Style::new())
        );
        plain(&line, Colours::None)
    };
    assert_eq!(bar(0), "░".repeat(20));
    assert_eq!(bar(30), format!("{}{}", "█".repeat(5), "░".repeat(15)));
    assert_eq!(bar(31), format!("{}{}", "█".repeat(5), "░".repeat(15)));
    assert_eq!(bar(120), "█".repeat(20));
    assert_eq!(bar(999), "█".repeat(20));
    assert_eq!(filled(20, Duration::from_secs(5), Duration::ZERO), 0);
    let full = Scene::new(
        model.snapshot(),
        Timestamp::UNIX_EPOCH,
        &zone,
        Colours::Full,
    );
    let line = progress_bar(&full, 4, Duration::from_secs(30), Duration::from_secs(120));
    assert_eq!(line.spans[0].style, Paint::Running.style(Colours::Full));
    assert_eq!(line.spans[1].style, Paint::Track.style(Colours::Full));
}

fn rich_bar(elapsed: u64) -> Line<'static> {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let scene = scene(
        &model.snapshot(),
        Timestamp::UNIX_EPOCH,
        &zone,
        Colours::Rich,
    );
    progress_bar(
        &scene,
        20,
        Duration::from_secs(elapsed),
        Duration::from_secs(160),
    )
}

#[test_case(0, ""; "nothing elapsed")]
#[test_case(1, "▏"; "one eighth")]
#[test_case(4, "▌"; "half a cell")]
#[test_case(7, "▉"; "seven eighths")]
#[test_case(8, "█"; "one whole cell")]
#[test_case(13, "█▋"; "a cell and five eighths")]
#[test_case(159, "███████████████████▉"; "just short of the end")]
#[test_case(160, "████████████████████"; "the end")]
fn a_rich_bar_ends_in_an_eighth_block_leading_edge(elapsed: u64, filled: &str) {
    let line = rich_bar(elapsed);
    let cells = filled.chars().count();
    assert_eq!(
        plain(&line, Colours::Rich),
        format!("{filled}{}", "░".repeat(20 - cells))
    );
    assert_eq!(
        line.spans[0].style,
        Paint::Running.style(Colours::Rich),
        "the leading edge is drawn in the bar's colour"
    );
    assert_eq!(line.spans.len(), 2, "equal cells share a span");
    assert_eq!(line.spans[1].style, Paint::Track.style(Colours::Rich));
}

#[test_case(Colours::Full; "full colour")]
#[test_case(Colours::Console; "console")]
fn only_the_rich_profile_draws_an_eighth_block_edge(colours: Colours) {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let scene = scene(&model.snapshot(), Timestamp::UNIX_EPOCH, &zone, colours);
    let line = progress_bar(
        &scene,
        20,
        Duration::from_secs(13),
        Duration::from_secs(160),
    );
    assert_eq!(
        plain(&line, colours),
        format!("█{}", "░".repeat(19)),
        "elapsed time rounds down to whole cells"
    );
}

#[test]
fn large_digits_are_three_rows_and_a_colon() {
    let rows = big_digits("1:05");
    assert_eq!(rows[0], "▀█    █▀█ █▀▀");
    assert!(
        rows.iter()
            .all(|row| row.chars().count() == rows[0].chars().count())
    );
    assert_eq!(
        big_digits("x"),
        [String::new(), String::new(), String::new()]
    );
}

#[test_case(0, Duration::MAX, Duration::MAX, 0; "empty track")]
#[test_case(65_535, Duration::MAX, Duration::MAX, 65_535; "largest terminal track")]
#[test_case(usize::MAX, Duration::MAX, Duration::MAX, 65_535; "oversized track clips")]
#[test_case(65_535, Duration::ZERO, Duration::MAX, 0; "long duration starts empty")]
fn progress_arithmetic_keeps_its_terminal_bound(
    cells: usize,
    elapsed: Duration,
    total: Duration,
    expected: usize,
) {
    assert_eq!(filled(cells, elapsed, total), expected);
}

#[test]
fn an_idle_hold_pins_the_clock_to_the_trials_start() {
    let start = at("2026-10-12T10:00:00Z");
    let running = trial(Regime::R6, Some(start), 600);
    let model = Model::new();
    let zone = TimeZone::UTC;
    let view = session(start);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.trial = Some(&running);
    let scene = scene(&s, at("2026-10-12T11:00:00Z"), &zone, Colours::None);
    assert_eq!(
        scene.hold,
        Some(IdleHold {
            started: start,
            planned_end: at("2026-10-12T10:10:00Z"),
        })
    );
    let text = plain(&header(&scene, 240, false), Colours::None);
    assert!(text.starts_with("togi   10:00:00"), "{text}");
    assert!(text.contains("screen paused · planned end 10:10"), "{text}");
    assert!(!text.contains("paused until"), "{text}");
    assert!(!text.contains("ago"), "{text}");
}

fn frame_in(hand: &Hand, width: u16, height: u16, now: Timestamp, colours: Colours) -> Buffer {
    let sc = screen(View::Main, width, height, colours, true);
    draw_checked(&sc, |sc, buf| {
        crate::frames::render(&hand.journal(), sc, now, &TimeZone::UTC, buf)
    })
    .0
}

fn symbol(buf: &Buffer, x: usize, y: usize) -> &str {
    buf[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())].symbol()
}

fn foreground(buf: &Buffer, x: usize, y: usize) -> ratatui::style::Color {
    buf[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())].fg
}

fn edge_hand(applied: i64, edge: i64) -> Hand {
    let projection = with_cap(
        ccd_row(0, &[0], 0),
        cap(-50, rate_bound(), fallback(), 1, 0),
        Some(observed(edge)),
        3,
    );
    let mut hand = Hand::new(
        vec![core_row(0, 0, Some(applied), false)],
        bare(vec![projection, ccd_row(1, &[8], 0)]),
    );
    hand.cores.push(core_row(8, 1, Some(0), false));
    hand
}

/// The edge occupies its own count, touching a matching-height applied segment.
#[test_case(240, 67, Colours::Rich, -27; "wide rich odd")]
#[test_case(240, 67, Colours::Rich, -28; "wide rich even")]
#[test_case(160, 45, Colours::Rich, -27; "medium rich odd")]
#[test_case(160, 45, Colours::Rich, -28; "medium rich even")]
#[test_case(240, 67, Colours::Console, -27; "wide console odd")]
#[test_case(240, 67, Colours::Console, -28; "wide console even")]
#[test_case(160, 45, Colours::Console, -27; "medium console odd")]
#[test_case(160, 45, Colours::Console, -28; "medium console even")]
fn the_rendered_frame_puts_each_edge_on_its_count(
    width: u16,
    height: u16,
    colours: Colours,
    edge: i64,
) {
    let applied = edge + 1;
    let hand = edge_hand(applied, edge);
    let zone = TimeZone::UTC;
    let snapshot = hand.snapshot();
    let probe = Scene::new(snapshot, FIXED_NOW, &zone, colours);
    let measured = measure(&probe, &screen(View::Main, width, height, colours, true));
    let table = measured.ccds[0];
    let buf = frame_in(&hand, width, height, FIXED_NOW, colours);
    let rows = rows_of(&buf);
    let y = usize::from(table.rect.y) + 3;
    let bad = Paint::Bad.style(colours).fg.expect("a colour");
    let glyph = marks(colours).applied.to_string();
    let at = |depth: i64| {
        usize::from(table.rect.x) + table.columns.gauge + usize::try_from(depth).unwrap() - 1
    };
    let mark = at(-edge);
    assert_eq!(symbol(&buf, mark, y), glyph, "edge {edge}: {:?}", rows[y]);
    assert_eq!(foreground(&buf, mark, y), bad, "edge {edge} is red");
    assert_eq!(
        symbol(&buf, mark - 1, y),
        glyph,
        "applied {applied} touches it"
    );
    assert_ne!(foreground(&buf, mark - 1, y), bad, "applied is not red");
    assert_eq!(symbol(&buf, at(1), y), glyph);
}

#[test_case(240, 67, Colours::Rich, 21, 50; "wide rich")]
#[test_case(160, 45, Colours::Rich, 20, 50; "medium rich")]
#[test_case(240, 67, Colours::Console, 21, 50; "wide console")]
#[test_case(160, 45, Colours::Console, 20, 50; "medium console")]
#[test_case(80, 24, Colours::Rich, 20, 50; "compact rich where fifty cells fit")]
#[test_case(70, 24, Colours::Rich, 21, 25; "compact rich where they do not")]
#[test_case(80, 24, Colours::Console, 20, 50; "compact console where fifty cells fit")]
#[test_case(70, 24, Colours::Console, 21, 25; "compact console where they do not")]
fn gauge_geometry_depends_on_the_room_for_fifty_cells(
    width: u16,
    height: u16,
    colours: Colours,
    gauge: usize,
    cells: usize,
) {
    let hand = edge_hand(-10, -20);
    let zone = TimeZone::UTC;
    let scene = Scene::new(hand.snapshot(), FIXED_NOW, &zone, colours);
    let measured = measure(&scene, &screen(View::Main, width, height, colours, true));
    let columns = measured.ccds[0].columns;
    assert_eq!((columns.gauge, columns.cells), (gauge, cells));
}

fn running_hand() -> Hand {
    let mut hand = edge_hand(-10, -20);
    hand.trial = Some(running_trial(Regime::R1, &[0]));
    hand
}

/// The elapsed time that fills `eighths` eighths of a `cells`-cell bar of `total`.
fn elapsed_for(total: Duration, cells: usize, eighths: usize) -> SignedDuration {
    let nanos = (total.as_nanos() * eighths as u128).div_ceil(cells as u128 * 8);
    SignedDuration::from_nanos(i64::try_from(nanos).unwrap())
}

#[test_case(4; "half a cell")]
#[test_case(3; "three eighths")]
#[test_case(7; "seven eighths")]
fn the_rendered_wide_bar_has_two_full_height_rows_and_no_underline(extra: usize) {
    let hand = running_hand();
    let snapshot = hand.snapshot();
    let trial = snapshot.trial.expect("a running trial");
    let started = trial.started.expect("a started trial");
    let zone = TimeZone::UTC;
    let probe = Scene::new(snapshot, started, &zone, Colours::Rich);
    let sc = screen(View::Main, 240, 67, Colours::Rich, true);
    let now_rect = measure(&probe, &sc).now;
    let cells = usize::from(now_rect.width) - 23;
    let eighths = cells * 8 / 3 + extra;
    let (whole, edge) = (eighths / 8, eighths % 8);
    let at = started + elapsed_for(trial.duration, cells, eighths);
    let buf = frame_in(&hand, 240, 67, at, Colours::Rich);
    let rows = rows_of(&buf);
    let (x, top) = (usize::from(now_rect.x) + whole, usize::from(now_rect.y) + 3);
    let edges = ['▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    for y in [top, top + 1] {
        let expected = if edge == 0 { '░' } else { edges[edge - 1] };
        assert_eq!(rows[y].chars().nth(x), Some(expected), "row {y}");
        let paint = if edge > 0 {
            Paint::Running
        } else {
            Paint::Track
        };
        assert_eq!(
            foreground(&buf, x, y),
            paint.style(Colours::Rich).fg.expect("a colour"),
            "row {y}"
        );
    }
    let bottom: Vec<char> = rows[top + 2]
        .chars()
        .skip(usize::from(now_rect.x))
        .take(cells)
        .collect();
    assert!(bottom.iter().all(|&c| c == ' '), "{bottom:?}");
}

#[test_case(240, 67, Colours::Console; "wide console")]
#[test_case(240, 67, Colours::Rich; "wide rich")]
#[test_case(160, 45, Colours::Console; "medium console")]
#[test_case(160, 45, Colours::Rich; "medium rich")]
#[test_case(120, 33, Colours::Console; "compact console")]
#[test_case(120, 33, Colours::Rich; "compact rich")]
fn trial_bars_have_no_strip_under_their_track(width: u16, height: u16, colours: Colours) {
    let hand = running_hand();
    let snapshot = hand.snapshot();
    let trial = snapshot.trial.expect("a running trial");
    let now = trial.started.expect("a started trial") + SignedDuration::from_secs(10);
    let zone = TimeZone::UTC;
    let scene = Scene::new(snapshot, now, &zone, colours);
    let measured = measure(&scene, &screen(View::Main, width, height, colours, true));
    let buf = frame_in(&hand, width, height, now, colours);
    let rows = rows_of(&buf);
    let r = measured.now;
    let (top, count, cells) = if measured.layout.class == SizeClass::Wide {
        (r.y + 3, 2, r.width - 23)
    } else {
        let y = r.bottom() - 1;
        let cells = rows[usize::from(y)]
            .chars()
            .skip(usize::from(r.x))
            .take_while(|&c| c != ' ')
            .count();
        (y, 1, u16::try_from(cells).unwrap())
    };
    for y in top..top + count {
        let bar: String = rows[usize::from(y)]
            .chars()
            .skip(usize::from(r.x))
            .take(usize::from(cells))
            .collect();
        assert!(bar.contains('█') && bar.contains('░'), "{bar:?}");
        assert!(!bar.contains('▀'), "{bar:?}");
    }
    let below: String = rows[usize::from(top + count)]
        .chars()
        .skip(usize::from(r.x))
        .take(usize::from(cells))
        .collect();
    assert!(below.chars().all(|c| c == ' '), "{below:?}");
}
