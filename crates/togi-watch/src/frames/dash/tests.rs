use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::{TimeZone, offset};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use togi_journal::{DeadEndCondition, Event, Shutdown, ShutdownReason, Stamp};
use togi_machine::{
    BootId, CcdId, Condition, CoreSet, ObservedOffset, PerCore, Regime, WorkloadId,
};
use togi_tuner::{CombinationId, CyclePlan, Limit, R7CoreStatus, Reached};

use super::ccd::{core_table, cores_page, draw_core_overview, note_line, state_word};
use super::header::{header, status_header};
use super::now::{filled, progress_bar};
use super::r7::{r7_rows, top_cell};
use super::stage::stage_line;
use crate::frames::colours::Paint;
use crate::frames::fit::{line_width, plain};
use crate::frames::layout::SizeClass;
use crate::frames::scene::Scene;
use crate::model::{
    CoreEvidence, CoreState, CoreView, Count, CycleView, DeadEndView, IdleHold, Model, SessionView,
    Snapshot, StopView, TrialView,
};
use crate::screen::Colours;
use crate::tests::kit::{config_loaded, core, off, seq};
use crate::tuning::TuningClock;

fn at(text: &str) -> Timestamp {
    text.parse().expect("a timestamp")
}

fn core_view(id: u8, ccd: u8, state: CoreState) -> CoreView {
    CoreView {
        id: core(id),
        ccd: CcdId(ccd),
        state,
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
        evidence: CoreEvidence::default(),
    }
}

fn trial(regime: Regime, started: Option<Timestamp>, duration: u64) -> TrialView {
    TrialView {
        id: togi_journal::TrialId::from("t1".to_owned()),
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
        duration: Duration::from_secs(duration),
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
    plain(&header(
        &scene(s, now, &TimeZone::UTC, Colours::Full),
        width,
        compact,
    ))
}

#[test]
fn header_counts_agree_in_number() {
    let model = Model::new();
    let start = at("2026-10-12T10:00:00Z");
    let now = at("2026-10-12T10:01:00Z");
    let view = session(start);
    for (failures, crashes, want) in [
        (0, 0, "0 failures · 0 crashes"),
        (1, 1, "1 failure · 1 crash"),
        (2, 1, "2 failures · 1 crash"),
    ] {
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
        assert!(line_width(&line) <= width, "{width}: {}", plain(&line));
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

#[test]
fn the_header_shows_the_tuning_time_beside_the_session_age_and_freezes_both() {
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
    for width in [160, 80] {
        let text = header_text(&s, now, width, false);
        let tuning = text.find("tuning 1h20m");
        let age = text.find("session 2h20m");
        assert!(tuning.is_some() && tuning < age, "{width}: {text}");
    }
    let wide = header_text(&s, now, 160, false);
    assert!(
        wide.contains("last observed failure 10:20, 2h00m ago"),
        "{wide}"
    );
    for width in 18..=160 {
        let line = header(&scene(&s, now, &TimeZone::UTC, Colours::None), width, false);
        assert!(line_width(&line) <= width, "{width}: {}", plain(&line));
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
    let text = plain(&header(&held, 160, false));
    assert!(
        text.contains("tuning 1h15m") && text.contains("session 2h15m"),
        "{text}"
    );
}

#[test]
fn a_journal_that_is_not_folded_names_its_status_and_no_earlier_run() {
    let now = at("2026-10-12T10:00:00Z");
    let line = plain(&status_header(
        now,
        &TimeZone::UTC,
        Colours::Full,
        "starting",
        80,
    ));
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
    let text = plain(&header(&scene(&s, now, &east, Colours::Full), 240, false));
    assert!(text.contains("togi   19:30:45"), "{text}");
    assert!(text.contains("last observed failure 19:20"), "{text}");
    let line = plain(&status_header(now, &east, Colours::Full, "starting", 80));
    assert!(line.starts_with("togi   19:30:45"), "{line}");

    let running = trial(Regime::R6, Some(start), 600);
    s.trial = Some(&running);
    let held = scene(&s, at("2026-10-12T11:00:00Z"), &east, Colours::Full);
    assert_eq!(held.now, start);
    let head = plain(&header(&held, 240, true));
    assert!(head.starts_with("togi   19:00:00"), "{head}");
    assert!(head.contains("paused · planned end 19:10"), "{head}");
    assert!(!head.contains("paused until"), "{head}");
    let stage = plain(&stage_line(&held, 240, true));
    assert!(
        stage.contains("screen paused · planned end 19:10"),
        "{stage}"
    );
    assert!(!stage.contains("paused until"), "{stage}");
}

fn held(combination: u32) -> Option<Limit> {
    CombinationId::new(crate::tests::kit::nz(combination)).into_limit()
}

trait IntoLimit {
    fn into_limit(self) -> Option<Limit>;
}

impl IntoLimit for CombinationId {
    fn into_limit(self) -> Option<Limit> {
        Some(Limit::Reached(Reached::Combination(self)))
    }
}

#[test]
fn medium_core_notes_retain_combination_and_solo_offsets() {
    let model = Model::new();
    let s = model.snapshot();
    let zone = TimeZone::UTC;
    let scene = scene(&s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
    let mut at_limit = core_view(0, 0, CoreState::AtLimit);
    at_limit.solo = Some(off(-31));
    at_limit.holder = held(4);
    let mut gave = core_view(8, 1, CoreState::HasRoom);
    gave.solo = Some(off(-37));
    gave.gave_back = Some(CombinationId::new(crate::tests::kit::nz(1)));
    for (view, want) in [(&at_limit, "C4 · solo -31"), (&gave, "C1 · solo -37")] {
        let note = plain(&note_line(&scene, view, 25));
        assert!(note.ends_with(want), "{note}");
    }
}

#[test]
fn a_note_follows_the_core_in_the_order_the_dashboard_prefers() {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let scene = scene(
        &model.snapshot(),
        Timestamp::UNIX_EPOCH,
        &zone,
        Colours::None,
    );
    let text = |view: &CoreView| plain(&note_line(&scene, view, 60));
    let mut probe = core_view(0, 0, CoreState::Probe);
    probe.group_fails = vec![off(-30), off(-28)];
    assert_eq!(text(&probe), "group failed at -30 -28");
    let mut parked = core_view(0, 0, CoreState::Parked);
    parked.returns_to = Some(off(-25));
    assert_eq!(text(&parked), "returns to -25");
    let mut confirm = core_view(0, 0, CoreState::Confirm);
    confirm.confirm = Some(crate::model::Confirm {
        offset: off(-31),
        light: 2,
        heavy: 1,
        needed: 3,
    });
    assert_eq!(text(&confirm), "-31: light 2/3, heavy 1/3");
    let mut found = core_view(0, 0, CoreState::Found);
    found.solo = Some(off(-33));
    assert_eq!(text(&found), "solo -33");
    let mut search = core_view(0, 0, CoreState::Search);
    search.pass = Some(off(-20));
    search.next = Some(off(-24));
    assert_eq!(text(&search), "passed -20 · next -24");
    let mut queued = core_view(0, 0, CoreState::AtLimit);
    queued.queued = Some(crate::tests::kit::seq(9));
    assert_eq!(text(&queued), "reset queued");
    assert_eq!(text(&core_view(0, 0, CoreState::Waiting)), "");
}

#[test]
fn every_state_has_its_word() {
    assert_eq!(state_word(CoreState::Search), "SEARCH");
    assert_eq!(state_word(CoreState::HasRoom), "HAS ROOM");
    assert_eq!(state_word(CoreState::AtLimit), "AT LIMIT");
    assert_eq!(state_word(CoreState::Probe), "PROBE");
    assert_eq!(state_word(CoreState::Waiting), "WAITING");
    assert_eq!(state_word(CoreState::Confirm), "CONFIRM");
    assert_eq!(state_word(CoreState::Found), "FOUND");
    assert_eq!(state_word(CoreState::Suspect), "SUSPECT");
    assert_eq!(state_word(CoreState::Member), "MEMBER");
    assert_eq!(state_word(CoreState::Parked), "PARKED");
}

const OVERVIEW: Rect = Rect::new(0, 0, 77, 26);

fn drawn(cores: &[CoreView], stopped: Option<&StopView>, colours: Colours) -> Buffer {
    drawn_with(cores, stopped, None, colours)
}

fn drawn_with(
    cores: &[CoreView],
    stopped: Option<&StopView>,
    running: Option<&TrialView>,
    colours: Colours,
) -> Buffer {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let view = session(Timestamp::UNIX_EPOCH);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.cores = cores;
    s.stopped = stopped;
    s.trial = running;
    let scene = scene(&s, Timestamp::UNIX_EPOCH, &zone, colours);
    let mut buf = Buffer::empty(OVERVIEW);
    draw_core_overview(&mut buf, OVERVIEW, &scene, CcdId(0));
    buf
}

fn cells(buf: &Buffer, y: u16, from: u16, to: u16) -> String {
    (from..=to).map(|x| buf[(x, y)].symbol()).collect()
}

fn lane(buf: &Buffer, y: u16) -> String {
    cells(buf, y, 4, 53)
}

fn values(buf: &Buffer, y: u16) -> String {
    cells(buf, y, 55, 76).trim_end().to_owned()
}

fn filled_columns(lane: &str) -> Vec<usize> {
    lane.chars()
        .enumerate()
        .filter(|(_, c)| *c == '█')
        .map(|(at, _)| at)
        .collect()
}

/// A core whose record starts at row 2: the tuning profile `tune`, the recorded failure `fail`.
fn recorded(tune: i64, solo: Option<i64>, fail: Option<i64>) -> CoreView {
    CoreView {
        profile: off(tune),
        solo: solo.map(off),
        fail: fail.map(off),
        ..core_view(0, 0, CoreState::AtLimit)
    }
}

#[test]
fn a_failure_one_count_deeper_than_the_tune_is_its_own_cell_in_the_next_lane() {
    let buf = drawn(&[recorded(-1, Some(-2), Some(-2))], None, Colours::None);
    assert_eq!(filled_columns(&lane(&buf, 2)), [0]);
    assert_eq!(filled_columns(&lane(&buf, 3)), [1]);
    assert_eq!(buf[(4, 2)].symbol(), "█");
    assert_eq!(buf[(5, 3)].symbol(), "█");
    assert_eq!(lane(&buf, 4).trim(), "");
    assert_eq!(values(&buf, 3), "s-2 g1 f-2");
}

#[test]
fn a_failure_at_the_tune_is_the_same_column_in_its_own_lane() {
    let buf = drawn(&[recorded(-45, Some(-45), Some(-45))], None, Colours::None);
    assert_eq!(filled_columns(&lane(&buf, 2)), (0..45).collect::<Vec<_>>());
    assert_eq!(filled_columns(&lane(&buf, 3)), [44]);
    assert_eq!(values(&buf, 2), "tune -45 now   0");
    assert_eq!(values(&buf, 3), "s-45 g0 f-45");
}

#[test]
fn a_tune_of_zero_is_an_empty_lane_and_a_missing_failure_says_none() {
    let buf = drawn(&[recorded(0, None, None)], None, Colours::None);
    assert!(filled_columns(&lane(&buf, 2)).is_empty());
    assert!(filled_columns(&lane(&buf, 3)).is_empty());
    assert_eq!(values(&buf, 2), "tune   0 now   0");
    assert_eq!(values(&buf, 3), "s? g? f none");
}

#[test]
fn a_failure_at_zero_draws_no_cell_and_says_f0() {
    let buf = drawn(&[recorded(0, Some(-10), Some(0))], None, Colours::None);
    assert!(filled_columns(&lane(&buf, 3)).is_empty());
    assert_eq!(values(&buf, 3), "s-10 g10 f0");
}

#[test]
fn an_unobserved_offset_is_a_question_mark_never_a_zero() {
    let mut unobserved = recorded(-9, None, None);
    unobserved.applied = None;
    let buf = drawn(&[unobserved], None, Colours::None);
    assert_eq!(values(&buf, 2), "tune  -9 now   ?");
    assert_eq!(filled_columns(&lane(&buf, 2)), (0..9).collect::<Vec<_>>());
}

#[test]
fn the_deepest_and_shallowest_depths_fill_the_first_and_last_cells() {
    let buf = drawn(
        &[
            recorded(-50, Some(-50), Some(-50)),
            recorded(-1, None, Some(-1)),
        ],
        None,
        Colours::None,
    );
    assert_eq!(filled_columns(&lane(&buf, 2)), (0..50).collect::<Vec<_>>());
    assert_eq!(filled_columns(&lane(&buf, 3)), [49]);
    assert_eq!(buf[(53, 3)].symbol(), "█");
    assert_eq!(filled_columns(&lane(&buf, 5)), [0]);
    assert_eq!(filled_columns(&lane(&buf, 6)), [0]);
}

#[test]
fn an_observed_offset_never_moves_the_tune_bar_and_is_never_clamped() {
    let mut restored = recorded(-25, Some(-30), None);
    restored.applied = Some(ObservedOffset(0));
    let stopped = StopView {
        at: Timestamp::UNIX_EPOCH,
        reason: ShutdownReason::Signal,
        saved: true,
    };
    let after = drawn(&[restored.clone()], Some(&stopped), Colours::None);
    let before = drawn(
        &[CoreView {
            applied: Some(ObservedOffset(-25)),
            ..restored.clone()
        }],
        None,
        Colours::None,
    );
    assert_eq!(lane(&after, 2), lane(&before, 2));
    assert_eq!(filled_columns(&lane(&after, 2)).len(), 25);
    assert_eq!(values(&after, 2), "tune -25 now   0");
    assert_eq!(values(&before, 2), "tune -25 now -25");
    let wild = CoreView {
        applied: Some(ObservedOffset(-73)),
        ..restored
    };
    let buf = drawn(&[wild], None, Colours::None);
    assert_eq!(values(&buf, 2), "tune -25 now -73");
    assert_eq!(filled_columns(&lane(&buf, 2)).len(), 25);
}

#[test]
fn the_gap_is_tune_minus_solo_and_keeps_its_sign() {
    let shallower = drawn(&[recorded(-9, Some(-45), None)], None, Colours::None);
    assert_eq!(values(&shallower, 3), "s-45 g36 f none");
    let deeper = drawn(&[recorded(-50, Some(-45), None)], None, Colours::None);
    assert_eq!(values(&deeper, 3), "s-45 g-5 f none");
}

#[test]
fn a_queued_reset_and_a_current_failure_badge_both_records_beside_the_load_role() {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let view = session(Timestamp::UNIX_EPOCH);
    let running = trial(Regime::R1, Some(Timestamp::UNIX_EPOCH), 60);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.trial = Some(&running);
    let mut core = recorded(-20, Some(-25), Some(-21));
    core.loaded = true;
    core.queued = Some(seq(9));
    core.evidence.current_failure = true;
    let both = [core.clone()];
    s.cores = &both;
    let scene = scene(&s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
    let mut buf = Buffer::empty(OVERVIEW);
    draw_core_overview(&mut buf, OVERVIEW, &scene, CcdId(0));
    assert_eq!(cells(&buf, 2, 0, 2), "00!");
    assert_eq!(cells(&buf, 3, 0, 2), "!R ");
    assert_eq!(values(&buf, 4), "loaded");
    assert_eq!(lane(&buf, 4).trim(), "");
    let idle = drawn(
        &[CoreView {
            loaded: false,
            evidence: CoreEvidence::default(),
            ..core.clone()
        }],
        None,
        Colours::None,
    );
    assert_eq!(cells(&idle, 2, 0, 2), "00R");
    assert_eq!(cells(&idle, 3, 0, 2), " R ");
    assert_eq!(values(&idle, 4), "reset queued");
}

#[test]
fn roles_name_what_each_core_does() {
    let running = trial(Regime::R1, Some(Timestamp::UNIX_EPOCH), 60);
    let role = |core: CoreView, trial: Option<&TrialView>| {
        let buf = drawn_with(&[core], None, trial, Colours::None);
        values(&buf, 4)
    };
    let state = |state, loaded, judged| CoreView {
        loaded,
        judged,
        ..core_view(0, 0, state)
    };
    assert_eq!(
        role(state(CoreState::Search, true, true), Some(&running)),
        "judged"
    );
    assert_eq!(
        role(state(CoreState::Search, true, false), Some(&running)),
        "loaded"
    );
    assert_eq!(
        role(state(CoreState::Search, false, false), Some(&running)),
        "idle at 0"
    );
    assert_eq!(
        role(state(CoreState::Search, true, true), None),
        "idle at 0"
    );
    let mut away = state(CoreState::HasRoom, false, false);
    away.applied = Some(ObservedOffset(-5));
    assert_eq!(role(away, None), "idle");
    assert_eq!(
        role(state(CoreState::Suspect, true, true), Some(&running)),
        "held failure"
    );
    assert_eq!(
        role(state(CoreState::Member, true, true), Some(&running)),
        "held failure"
    );
    assert_eq!(
        role(state(CoreState::Probe, true, true), Some(&running)),
        "probing"
    );
    assert_eq!(
        role(state(CoreState::Parked, false, false), Some(&running)),
        "parked"
    );
}

#[test]
fn the_heading_row_carries_the_scale_and_the_label_row_the_columns() {
    let buf = drawn(&[], None, Colours::None);
    let heading = cells(&buf, 0, 0, 76);
    assert_eq!(&heading[3..4], "0");
    assert_eq!(&heading[5..10], "CCD 0");
    for (count, column) in [(10, 11), (20, 21), (30, 31), (40, 41), (50, 51)] {
        assert_eq!(&heading[column..column + 3], format!("-{count}"));
    }
    assert_eq!(cells(&buf, 1, 0, 13), "core  tune now");
    assert_eq!(cells(&buf, 1, 16, 49), "█ tune  █ fail  ! failing  R reset");
    assert_eq!(cells(&buf, 1, 55, 73), "s=solo g=gap f=fail");
}

#[test]
fn the_lane_and_its_neighbours_in_the_record_carry_only_their_own_marks() {
    let cores: Vec<CoreView> = (0..8)
        .map(|id| CoreView {
            id: core(id),
            ..recorded(-30, Some(-35), Some(-36))
        })
        .collect();
    let buf = drawn(&cores, None, Colours::None);
    for record in 0..8u16 {
        let top = 2 + 3 * record;
        assert_eq!(cells(&buf, top, 0, 1), format!("{record:02}"));
        assert_eq!(buf[(3, top)].symbol(), " ");
        assert_eq!(buf[(54, top)].symbol(), " ");
        assert_eq!(filled_columns(&lane(&buf, top)).len(), 30);
        assert_eq!(filled_columns(&lane(&buf, top + 1)), [35]);
        assert_eq!(lane(&buf, top + 2).trim(), "");
        assert_eq!(cells(&buf, top + 2, 0, 3).trim(), "");
        assert_eq!(values(&buf, top + 1), "s-35 g5 f-36");
    }
}

#[test]
fn the_overview_stays_inside_its_rectangle() {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let view = session(Timestamp::UNIX_EPOCH);
    let cores: Vec<CoreView> = (0..12)
        .map(|id| CoreView {
            id: core(id),
            evidence: crate::model::CoreEvidence {
                current_failure: true,
                ..Default::default()
            },
            queued: Some(seq(3)),
            ..recorded(-50, Some(-50), Some(-50))
        })
        .collect();
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.cores = &cores;
    let scene = scene(&s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
    let area = Rect::new(0, 0, 90, 34);
    let inside = Rect::new(6, 4, 77, 26);
    let mut buf = Buffer::filled(area, ratatui::buffer::Cell::new("~"));
    draw_core_overview(&mut buf, inside, &scene, CcdId(0));
    for y in 0..area.height {
        for x in 0..area.width {
            if !inside.contains(ratatui::layout::Position::new(x, y)) {
                assert_eq!(buf[(x, y)].symbol(), "~", "({x}, {y})");
            }
        }
    }
    assert_eq!(buf[(6 + 4, 4 + 2)].symbol(), "█");
    let last = 4 + 2 + 3 * 7;
    assert_eq!(buf[(6 + 53, last + 1)].symbol(), "█");
}

#[test]
fn colours_tell_a_current_failure_from_a_recorded_one_and_monochrome_keeps_the_shapes() {
    let mut current = recorded(-10, Some(-12), Some(-11));
    current.evidence.current_failure = true;
    let recorded_core = recorded(-10, Some(-12), Some(-11));
    for colours in [Colours::Full, Colours::Console] {
        let now = drawn(&[current.clone()], None, colours);
        let then = drawn(std::slice::from_ref(&recorded_core), None, colours);
        assert_eq!(now[(14, 3)].symbol(), "█");
        assert_eq!(now[(14, 3)].fg, Paint::Bad.style(colours).fg.expect("fg"));
        assert_eq!(
            then[(14, 3)].fg,
            Paint::Recorded.style(colours).fg.expect("fg")
        );
        assert_eq!(then[(4, 2)].fg, Paint::White.style(colours).fg.expect("fg"));
        assert_ne!(now[(14, 3)].fg, then[(14, 3)].fg);
    }
    let plain = drawn(&[current], None, Colours::None);
    assert_eq!(plain[(14, 3)].symbol(), "█");
    assert_eq!(plain[(14, 3)].fg, ratatui::style::Color::Reset);
    assert_eq!(plain[(4, 2)].symbol(), "█");
    assert_eq!(values(&plain, 3), "s-12 g2 f-11");
}

fn table_text(cores: &[CoreView], width: usize) -> Vec<String> {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let view = session(Timestamp::UNIX_EPOCH);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.cores = cores;
    let scene = scene(&s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
    core_table(&scene, width).iter().map(plain).collect()
}

#[test]
fn the_compact_table_lists_every_core_in_ccd_and_core_order_with_exact_values() {
    let mut cores: Vec<CoreView> = Vec::new();
    for id in 0..16u8 {
        cores.push(CoreView {
            id: core(id),
            ccd: CcdId(id % 2),
            ..recorded(-(i64::from(id)), Some(-30), (id == 3).then_some(-31))
        });
    }
    cores[1].applied = None;
    cores[5].queued = Some(seq(4));
    let rows = table_text(&cores, 79);
    assert_eq!(rows.len(), 17);
    assert_eq!(rows[0], "CCD core tune  now solo  gap fail  role");
    assert_eq!(rows[1], "  0   00    0    0  -30   30 none  idle at 0");
    assert_eq!(rows[8], "  0   14  -14    0  -30   16 none  idle at 0");
    assert_eq!(rows[9], "  1   01   -1    ?  -30   29 none  idle");
    assert_eq!(rows[10], "  1   03   -3    0  -30   27  -31  idle at 0");
    assert_eq!(rows[11], "  1   05   -5    0  -30   25 none  reset queued");
    let order: Vec<&str> = rows[1..].iter().map(|row| &row[..8]).collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(order, sorted);
}

#[test]
fn a_narrow_table_drops_the_role_words_before_any_number() {
    let cores = [CoreView {
        solo: Some(off(-45)),
        fail: Some(off(0)),
        queued: Some(seq(2)),
        ..core_view(2, 1, CoreState::AtLimit)
    }];
    let wide = table_text(&cores, 79);
    assert_eq!(wide[1], "  1   02    0    0  -45   45    0  reset queued");
    let narrow = table_text(&cores, 33);
    assert_eq!(narrow[0], "CCD core tune  now solo  gap fail");
    assert_eq!(narrow[1], "  1   02    0    0  -45   45    0");
}

#[test]
fn a_table_too_narrow_for_every_role_word_keeps_the_failure_and_reset_badges() {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let view = session(Timestamp::UNIX_EPOCH);
    let running = trial(Regime::R1, Some(Timestamp::UNIX_EPOCH), 60);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.trial = Some(&running);
    let mut loaded = recorded(-20, Some(-25), Some(-21));
    loaded.loaded = true;
    loaded.queued = Some(seq(9));
    loaded.evidence.current_failure = true;
    let held = CoreView {
        state: CoreState::Suspect,
        ..loaded.clone()
    };
    let table = |core: &CoreView, width| {
        let cores = [core.clone()];
        let mut snapshot = s;
        snapshot.cores = &cores;
        let scene = scene(&snapshot, Timestamp::UNIX_EPOCH, &zone, Colours::None);
        plain(&core_table(&scene, width)[1])
    };
    let numbers = "  0   00  -20    0  -25    5  -21";
    assert_eq!(
        table(&loaded, 79),
        format!("{numbers}  loaded · failing now · reset queued")
    );
    assert_eq!(table(&loaded, 60), format!("{numbers}  !R loaded"));
    assert_eq!(table(&held, 47), format!("{numbers}  !R"));
    let only_queued = CoreView {
        evidence: CoreEvidence::default(),
        ..loaded
    };
    assert_eq!(table(&only_queued, 47), format!("{numbers}  R loaded"));
}

#[test]
fn the_cores_page_explains_each_value_and_where_the_failure_came_from() {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let view = session(Timestamp::UNIX_EPOCH);
    let mut gapped = recorded(-9, Some(-45), Some(-46));
    gapped.state = CoreState::HasRoom;
    gapped.evidence = crate::model::CoreEvidence {
        failure: Some(crate::model::FailureEvidence {
            offset: off(-46),
            source: crate::model::FailureSource::R7Charged,
            seq: seq(812),
        }),
        decision: Some(crate::model::OffsetDecision {
            seq: seq(812),
            decision: togi_journal::Decision::Backoff,
            activity: togi_journal::Activity::Checking,
            from: off(-46),
            to: off(-9),
        }),
        current_failure: true,
    };
    let mut zero = CoreView {
        id: core(1),
        applied: None,
        ..recorded(0, None, Some(0))
    };
    zero.queued = Some(seq(5));
    let none = CoreView {
        id: core(2),
        ..recorded(-3, Some(-3), None)
    };
    let deeper = CoreView {
        id: core(3),
        ..recorded(-50, Some(-45), None)
    };
    let cores = [gapped, zero, none, deeper];
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.cores = &cores;
    let scene = scene(&s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
    let page: Vec<String> = cores_page(&scene, 100).iter().map(plain).collect();
    let text = page.join("\n");
    for want in [
        "Core 00 · CCD 0 · HAS ROOM",
        "  tune -9 · now 0",
        "  solo -45 · gap 36: 36 counts shallower than solo",
        "  failure -46 · R7-charged · journal #812",
        "  ! current failure: recorded in the step now running",
        "  room: eligible to deepen in phase 2",
        "  last decision: backoff -46 to -9 while checking, journal #812",
        "  tune 0 · now ? (no readback recorded)",
        "  solo ? · gap ?: no solo limit recorded yet",
        "  failure at 0 · source not recorded",
        "  R reset queued: command #5, the core searches again at the next run",
        "  last decision: none recorded",
        "  solo -3 · gap 0: at the solo limit",
        "  solo -45 · gap -5: 5 counts deeper than solo",
        "  no failure recorded",
    ] {
        assert!(page.iter().any(|line| line == want), "{want:?} in\n{text}");
    }
    let narrow = cores_page(&scene, 30);
    assert!(narrow.iter().all(|line| line_width(line) <= 30));
    let none: [CoreView; 0] = [];
    let mut bare = model.snapshot();
    bare.cores = &none;
    let bare = Scene::new(bare, Timestamp::UNIX_EPOCH, &zone, Colours::None);
    assert_eq!(
        plain(&cores_page(&bare, 60)[0]),
        "No cores yet: the journal records no session to show."
    );
}

fn ids(values: &[u8]) -> togi_machine::CoreSet {
    values.iter().map(|&id| core(id)).collect()
}

fn r7_fixture(
    statuses: &[R7CoreStatus],
    class: SizeClass,
    width: usize,
    with_cycle: bool,
) -> Vec<String> {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let plan = CyclePlan {
        number: None,
        open: true,
        steps: Vec::new(),
        current: 0,
        paused: false,
    };
    let mut s = model.snapshot();
    s.r7 = statuses;
    if with_cycle {
        s.cycle = Some(CycleView {
            plan: &plan,
            step_hunts: &[],
            resume: None,
        });
    }
    let scene = scene(&s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
    r7_rows(&scene, class, width).iter().map(plain).collect()
}

fn r7_status(core_id: u8, top: bool, proxy: bool, own: bool) -> R7CoreStatus {
    R7CoreStatus {
        core: core(core_id),
        ccd: CcdId(core_id / 8),
        workload: WorkloadId::new(Regime::R7, 0).expect("a workload"),
        top_requester: top,
        offset_proxy: proxy,
        self_sufficient: own,
        passes: 0,
    }
}

#[test]
fn r7_lines_name_top_requesters_and_self_sufficiency() {
    let statuses = [
        r7_status(3, true, false, true),
        r7_status(7, false, false, false),
    ];
    for (class, width) in [(SizeClass::Wide, 115), (SizeClass::Compact, 57)] {
        let lines = r7_fixture(&statuses, class, width, true);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("self-sufficient"), "{lines:?}");
        assert!(lines[1].contains("mprime AVX2"), "{lines:?}");
        assert_eq!(lines[1].matches("03").count(), 2, "{lines:?}");
        assert!(!lines[1].contains("07"), "{lines:?}");
        assert_eq!(
            class == SizeClass::Wide,
            lines[0].contains("not a guarantee"),
            "{lines:?}"
        );
    }
    let proxy = [r7_status(3, true, true, true)];
    let lines = r7_fixture(&proxy, SizeClass::Compact, 57, true);
    assert!(lines[1].contains("03 by offset"), "{lines:?}");
    assert!(r7_fixture(&statuses, SizeClass::Compact, 57, false).is_empty());
    let narrow = r7_fixture(&proxy, SizeClass::Compact, 24, true);
    assert!(narrow[1].contains("by offset"), "{narrow:?}");
    assert!(narrow[1].is_ascii(), "{narrow:?}");
}

#[test]
fn cut_top_requesters_keep_their_offset_label() {
    let many: Vec<_> = (0..16)
        .map(|id| r7_status(id, id % 2 == 0, true, false))
        .collect();
    for (class, width) in [
        (SizeClass::Wide, 115),
        (SizeClass::Medium, 80),
        (SizeClass::Compact, 57),
    ] {
        let lines = r7_fixture(&many, class, width, true);
        assert!(lines[1].contains("... by offset"), "{width}: {lines:?}");
    }
    let mixed: Vec<_> = (0..16)
        .map(|id| r7_status(id, (id % 2 == 0 && id < 8) || id == 8, id >= 8, false))
        .collect();
    for (class, width) in [
        (SizeClass::Wide, 115),
        (SizeClass::Medium, 80),
        (SizeClass::Compact, 57),
    ] {
        let row = &r7_fixture(&mixed, class, width, true)[1];
        assert!(
            row.contains("08 by offset") && !row.contains("06 by offset"),
            "{row:?}"
        );
    }
    assert_eq!(
        top_cell(ids(&[0]), ids(&[8, 9, 10, 11, 12, 13, 14, 15]), 17),
        "08-15 by offset"
    );
    assert_eq!(top_cell(ids(&[]), ids(&[]), 17), "-");
}

#[test]
fn r7_values_stay_whole() {
    let rows = |measured: &[u8], proxies: &[u8], class, width, name: usize, top: usize| {
        let mut statuses: Vec<_> = measured
            .iter()
            .map(|&id| r7_status(id, true, false, false))
            .collect();
        statuses.extend(proxies.iter().map(|&id| r7_status(id, true, true, false)));
        let lines = r7_fixture(&statuses, class, width, true);
        assert!(
            line_width(&Line::from(lines[1].as_str())) <= width,
            "{lines:?}"
        );
        lines[1][name + 1..name + 1 + top].trim().to_owned()
    };
    let all: Vec<u8> = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13, 14, 15];
    let eight: Vec<u8> = (8..16).collect();
    let seven: Vec<u8> = (0..7).collect();
    assert_eq!(rows(&[], &all, SizeClass::Compact, 57, 17, 17), "by offset");
    assert_eq!(
        rows(&[], &all, SizeClass::Medium, 80, 24, 20),
        "00-10... by offset"
    );
    assert_eq!(
        rows(&[], &all, SizeClass::Wide, 115, 30, 24),
        "00-10 12-15 by offset"
    );
    assert_eq!(
        rows(&seven, &eight, SizeClass::Compact, 57, 17, 17),
        "08-15 by offset"
    );
    assert_eq!(
        rows(&seven, &eight, SizeClass::Medium, 80, 24, 20),
        "08-15 by offset"
    );
    assert_eq!(
        rows(&seven, &eight, SizeClass::Wide, 115, 30, 24),
        "00-06; 08-15 by offset"
    );
    assert_eq!(
        rows(
            &[0, 2, 4, 6, 8, 10, 12, 14],
            &[],
            SizeClass::Compact,
            57,
            17,
            17
        ),
        "00 02 04 06 08..."
    );
}

#[test]
fn narrow_r7_rows_keep_offset_provenance_and_fit() {
    let proxies: Vec<_> = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13, 14, 15]
        .into_iter()
        .map(|id| r7_status(id, true, true, false))
        .collect();
    for width in [24, 30, 35, 40] {
        let rows = r7_fixture(&proxies, SizeClass::Compact, width, true);
        assert!(rows[1].contains("by offset"), "{width}: {:?}", rows[1]);
        for cut in ["00-0...", "00-1...", "12-1..."] {
            assert!(!rows[1].contains(cut), "{width}: {:?}", rows[1]);
        }
        for word in rows[0].split_whitespace() {
            assert!(
                word.trim_matches('─').is_empty()
                    || ["R7", "top", "requesters", "self-sufficient"].contains(&word),
                "{width}: {:?}",
                rows[0]
            );
        }
        for row in &rows {
            assert!(
                line_width(&Line::from(row.as_str())) <= width,
                "{width}: {row:?}"
            );
        }
    }
    for (width, want) in [(8, ""), (9, "by offset"), (10, "by offset")] {
        let rows = r7_fixture(&proxies, SizeClass::Compact, width, true);
        assert_eq!(rows[1].trim(), want, "{width}");
        for row in &rows {
            assert!(
                line_width(&Line::from(row.as_str())) <= width,
                "{width}: {row:?}"
            );
        }
    }
}

#[test]
fn every_r7_workload_has_its_row() {
    let ids: Vec<_> = (0..togi_machine::workloads(Regime::R7).len())
        .filter_map(|i| WorkloadId::new(Regime::R7, i))
        .collect();
    let statuses: Vec<_> = ids
        .iter()
        .map(|&workload| R7CoreStatus {
            core: core(0),
            ccd: CcdId(0),
            workload,
            top_requester: true,
            offset_proxy: false,
            self_sufficient: false,
            passes: 0,
        })
        .collect();
    let lines = r7_fixture(&statuses, SizeClass::Compact, 57, true);
    assert_eq!(lines.len(), ids.len() + 1);
    for row in &lines[1..] {
        assert!(row.contains("none yet") && row.contains("00"), "{row:?}");
    }
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
        let line = progress_bar(
            &scene,
            20,
            filled(20, Duration::from_secs(elapsed), total),
            "█",
            "░",
        );
        assert!(
            line.spans
                .iter()
                .all(|span| span.style == ratatui::style::Style::new())
        );
        plain(&line)
    };
    assert_eq!(bar(0), "░".repeat(20));
    assert_eq!(bar(30), format!("{}{}", "█".repeat(5), "░".repeat(15)));
    assert_eq!(bar(120), "█".repeat(20));
    assert_eq!(bar(999), "█".repeat(20));
    assert_eq!(filled(20, Duration::from_secs(5), Duration::ZERO), 0);
    let full = Scene::new(
        model.snapshot(),
        Timestamp::UNIX_EPOCH,
        &zone,
        Colours::Full,
    );
    let line = progress_bar(&full, 4, 1, "█", "░");
    assert_eq!(line.spans[0].style, Paint::Running.style(Colours::Full));
    assert_eq!(line.spans[1].style, Paint::Track.style(Colours::Full));
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
    let text = plain(&header(&scene, 240, false));
    assert!(text.starts_with("togi   10:00:00"), "{text}");
    assert!(text.contains("screen paused · planned end 10:10"), "{text}");
    assert!(!text.contains("paused until"), "{text}");
    assert!(!text.contains("ago"), "{text}");
}

#[test]
fn an_idle_hold_freezes_both_durations_at_the_trials_start_through_its_later_events() {
    use crate::tests::kit::{Script, warning};
    use togi_journal::Outcome;
    use togi_tuner::Action;

    fn append_at(script: &mut Script, payload: togi_journal::Payload, time: Timestamp) {
        let mut event = script.event(payload, &[]);
        event.time = time;
        script.shadow.fold(&event).unwrap();
        script.events.push(event);
    }
    fn shown(script: &Script, now: Timestamp) -> String {
        let model = script.model();
        let zone = TimeZone::UTC;
        let scene = scene(&model.snapshot(), now, &zone, Colours::None);
        assert!(scene.hold.is_some(), "the idle trial holds the frame");
        plain(&header(&scene, 160, false))
    }

    let mut script = Script::at_limit();
    let mut idle = None;
    for _ in 0..200 {
        if let Ok(Action::RunTrial(plan)) = script.shadow.next() {
            if plan.regime == Regime::R6 {
                idle = Some(plan);
                break;
            }
        }
        script.step(|_| Outcome::Pass);
    }
    let plan = idle.expect("the cycle reaches its idle step");
    let config = script
        .events
        .iter()
        .find(|event| event.kind() == togi_journal::EventKind::ConfigLoaded)
        .expect("the run's configuration")
        .time;
    let (intent, trial) = script.intent(&plan);
    let hours = |n: i64| jiff::SignedDuration::from_secs(n * 3600);
    let started = config.checked_add(hours(2)).unwrap();
    script.start(&trial);
    script.events.last_mut().unwrap().time = started;

    let first = shown(&script, started);
    assert!(
        first.contains("tuning 2h00m") && first.contains("session 2h00m"),
        "{first}"
    );
    let progress = togi_journal::TrialProgress {
        trial: trial.clone(),
        detail: "still running".to_owned(),
        signal: None,
        core: None,
    };
    append_at(
        &mut script,
        progress.into(),
        started.checked_add(hours(1)).unwrap(),
    );
    append_at(
        &mut script,
        warning("a session event during the hold"),
        started.checked_add(hours(3)).unwrap(),
    );
    assert!(script.model().snapshot().idle_hold().is_some());
    for now in [started, started.checked_add(hours(5)).unwrap()] {
        let held = shown(&script, now);
        assert!(
            held.contains("tuning 2h00m") && held.contains("session 2h00m"),
            "both durations stay at the trial's start: {held}"
        );
    }

    let ended = started.checked_add(hours(4)).unwrap();
    let end = script.end(intent, &trial, Outcome::Pass);
    script.events[usize::try_from(end.get()).unwrap() - 1].time = ended;
    let model = script.model();
    let snap = model.snapshot();
    assert!(
        snap.idle_hold().is_none(),
        "the recorded end releases the hold"
    );
    assert_eq!(
        snap.tuning.at(ended),
        Duration::from_hours(6),
        "the accumulator advanced behind the hold and counts through the trial's end"
    );
}
