use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::{TimeZone, offset};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use togi_journal::ShutdownReason;
use togi_machine::{CcdId, Condition, CoreSet, ObservedOffset, PerCore, Regime, WorkloadId};
use togi_tuner::{CombinationId, CyclePlan, Limit, R7CoreStatus, Reached};

use super::ccd::{ccd_role, gauge, note_line, state_word};
use super::header::{header, status_header};
use super::now::{big_digits, filled, progress_bar, requirement_line};
use super::r7::{r7_rows, top_cell};
use super::stage::stage_line;
use crate::frames::colours::Paint;
use crate::frames::fit::{line_width, plain};
use crate::frames::layout::SizeClass;
use crate::frames::measure::{Ccd, CoreColumns};
use crate::frames::notes::top_note;
use crate::frames::scene::Scene;
use crate::frames::testing::rows_of;
use crate::model::{
    CoreState, CoreView, Count, CycleView, IdleHold, Model, SessionView, Snapshot, StopView,
    TrialView,
};
use crate::screen::Colours;
use crate::tests::kit::{core, off};

fn at(text: &str) -> Timestamp {
    text.parse().expect("a timestamp")
}

fn core_view(id: u8, ccd: u8, state: CoreState) -> CoreView {
    CoreView {
        id: core(id),
        ccd: CcdId(ccd),
        state,
        queued: None,
        applied: ObservedOffset(0),
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
        assert!(text.contains("paused until"), "{text}");
        assert!(text.contains(want), "{text}");
    }
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
    let narrow = header_text(&s, now, 46, false);
    assert!(narrow.contains("3 failures · 1 crash"), "{narrow}");
    assert!(!narrow.contains("last observed"), "{narrow}");
    for width in 18..60 {
        let line = header(&scene(&s, now, &TimeZone::UTC, Colours::None), width, false);
        assert!(line_width(&line) <= width, "{width}: {}", plain(&line));
    }
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
    assert!(head.contains("paused until 19:10"), "{head}");
    let stage = plain(&stage_line(&held, SizeClass::Wide, true));
    assert!(stage.contains("screen paused until 19:10"), "{stage}");
}

#[test]
fn the_stage_line_marks_what_runs_and_what_stopped() {
    let model = Model::new();
    let start = at("2026-10-12T10:00:00Z");
    let view = session(start);
    let cores = [
        core_view(0, 0, CoreState::Search),
        core_view(1, 0, CoreState::Waiting),
    ];
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.cores = &cores;
    let stage = |s: Snapshot<'_>, class| {
        plain(&stage_line(
            &scene(&s, start, &TimeZone::UTC, Colours::Full),
            class,
            false,
        ))
    };
    let running = stage(s, SizeClass::Wide);
    assert!(
        running.starts_with("► SOLO LIMITS  0 of 2 found"),
        "{running}"
    );
    assert!(running.contains("○ PHASE 1"), "{running}");
    assert!(running.contains("CLEAN CYCLES 0"), "{running}");
    let short = stage(s, SizeClass::Compact);
    assert!(short.starts_with("► SOLO  0 of 2 found"), "{short}");
    assert!(short.contains(" ─ "), "{short}");
    let stopped = StopView {
        at: start,
        reason: ShutdownReason::Signal,
        saved: false,
    };
    s.stopped = Some(&stopped);
    let after = stage(s, SizeClass::Wide);
    assert!(after.starts_with("○ SOLO LIMITS"), "{after}");
    assert!(!after.contains('►'), "{after}");
    let none = stage_line(
        &scene(
            &Model::new().snapshot(),
            start,
            &TimeZone::UTC,
            Colours::Full,
        ),
        SizeClass::Wide,
        false,
    );
    assert_eq!(line_width(&none), 0);
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
        plain(&stage_line(
            &scene(&s, at(now), &TimeZone::UTC, Colours::Full),
            SizeClass::Medium,
            true,
        ))
    };
    assert!(line("2026-10-12T10:04:00Z", false).ends_with("6:00 left"));
    assert!(line("2026-10-12T10:11:00Z", false).ends_with("1:00 past its end"));
    assert!(line("2026-10-12T10:04:00Z", true).ends_with("trial crashed"));
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
}

#[test]
fn a_ccd_role_counts_load_and_probes() {
    let model = Model::new();
    let zone = TimeZone::UTC;
    for (name, cores, want) in [
        (
            "parked with a load left",
            vec![
                CoreView {
                    loaded: true,
                    ..core_view(0, 0, CoreState::Parked)
                },
                core_view(1, 0, CoreState::Parked),
            ],
            "parked at 0, 1 under load",
        ),
        (
            "parked and idle",
            vec![
                core_view(0, 0, CoreState::Parked),
                core_view(1, 0, CoreState::Parked),
            ],
            "parked at 0, idle",
        ),
        (
            "a member probed",
            vec![
                CoreView {
                    loaded: true,
                    ..core_view(0, 0, CoreState::Member)
                },
                CoreView {
                    loaded: true,
                    ..core_view(1, 0, CoreState::Probe)
                },
            ],
            "members, one probed shallower than its failing offset",
        ),
    ] {
        let mut s = model.snapshot();
        s.cores = &cores;
        let scene = scene(&s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
        assert_eq!(ccd_role(&scene, CcdId(0)).0, want, "{name}");
    }
    let mut s = model.snapshot();
    let none: [CoreView; 0] = [];
    s.cores = &none;
    let scene = scene(&s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
    assert_eq!(ccd_role(&scene, CcdId(0)), (String::new(), String::new()));
}

fn gauge_text(core: &CoreView, applied: i64) -> String {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let scene = scene(
        &model.snapshot(),
        Timestamp::UNIX_EPOCH,
        &zone,
        Colours::None,
    );
    plain(&gauge(&scene, core, applied, 50))
        .trim_end()
        .to_owned()
}

#[test]
fn a_gauge_shows_the_depth_a_combination_holds() {
    let core = |profile: i64, solo: i64| CoreView {
        profile: off(profile),
        solo: Some(off(solo)),
        fail: Some(off(-45)),
        holder: held(4),
        ..core_view(0, 0, CoreState::AtLimit)
    };
    assert_eq!(
        gauge_text(&core(-43, -44), -43),
        format!("{}░█", "▄".repeat(43))
    );
    assert_eq!(
        gauge_text(&core(-43, -44), 0),
        format!("{}░█", "·".repeat(43))
    );
    assert_eq!(
        gauge_text(&core(-43, -43), -43),
        format!("{} █", "▄".repeat(43))
    );
}

#[test]
fn a_gauge_shows_group_failures_and_judging() {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let scene = scene(
        &model.snapshot(),
        Timestamp::UNIX_EPOCH,
        &zone,
        Colours::Full,
    );
    let mut core = core_view(0, 0, CoreState::Probe);
    core.group_fails = vec![off(-4)];
    let line = gauge(&scene, &core, -2, 25);
    assert!(plain(&line).starts_with("▄▀"), "{}", plain(&line));
    let painted = |judged: bool| {
        let mut core = core.clone();
        core.judged = judged;
        gauge(&scene, &core, -2, 25).spans[0].style
    };
    assert_eq!(painted(true), Paint::White.style(Colours::Full));
    assert_eq!(painted(false), Paint::Grey.style(Colours::Full));
    assert_eq!(line_width(&gauge(&scene, &core, 0, 0)), 0);
}

fn table(cores: &[CoreView], stopped: Option<&StopView>, width: u16, brief: bool) -> Vec<String> {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let view = session(Timestamp::UNIX_EPOCH);
    let mut s = model.snapshot();
    s.session = Some(&view);
    s.cores = cores;
    s.stopped = stopped;
    let scene = scene(&s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
    let columns = if brief {
        CoreColumns {
            id: 2,
            offset: 5,
            state: 9,
            gauge: 18,
            cells: 25,
            failure: 44,
            note: 50,
            brief: true,
        }
    } else {
        CoreColumns {
            id: 2,
            offset: 6,
            state: 11,
            gauge: 21,
            cells: 25,
            failure: 48,
            note: 56,
            brief: false,
        }
    };
    let rect = Rect::new(0, 0, width, 6);
    let mut buf = Buffer::empty(rect);
    super::draw_ccd(
        &mut buf,
        &Ccd {
            id: CcdId(0),
            rect,
            columns,
        },
        &scene,
    );
    rows_of(&buf)
}

#[test]
fn a_restored_session_shows_the_saved_profile_in_its_rows() {
    let mut row = core_view(0, 0, CoreState::HasRoom);
    row.profile = off(-25);
    row.applied = ObservedOffset(0);
    let cores = [row];
    let stopped = StopView {
        at: Timestamp::UNIX_EPOCH,
        reason: ShutdownReason::Signal,
        saved: true,
    };
    let saved = table(&cores, Some(&stopped), 80, false);
    assert!(saved[1].contains("00  -25"), "{:?}", saved[1]);
    assert!(saved[1].contains('▄'), "{:?}", saved[1]);
    let live = table(&cores, None, 80, false);
    assert!(live[1].contains("00    0"), "{:?}", live[1]);
    assert!(!live[1].contains('▄'), "{:?}", live[1]);
    let model = Model::new();
    let role = |stopped: Option<&StopView>| {
        let mut s = model.snapshot();
        s.cores = &cores;
        s.stopped = stopped;
        let zone = TimeZone::UTC;
        ccd_role(
            &scene(&s, Timestamp::UNIX_EPOCH, &zone, Colours::None),
            CcdId(0),
        )
        .0
    };
    assert_eq!(role(None), "waiting at 0");
    assert_eq!(role(Some(&stopped)), "not under load");
}

#[test]
fn a_readback_is_never_clamped() {
    let mut row = core_view(0, 0, CoreState::HasRoom);
    row.applied = ObservedOffset(-73);
    let text = table(&[row], None, 80, false);
    assert!(text[1].contains("-73"), "{:?}", text[1]);
}

#[test]
fn a_ccd_table_names_its_columns_and_counts_what_it_leaves_out() {
    let cores: Vec<_> = (0..6)
        .map(|id| core_view(id, 0, CoreState::Waiting))
        .collect();
    let text = table(&cores, None, 80, false);
    assert!(text[0].starts_with("CCD 0"), "{:?}", text[0]);
    assert!(
        text[0].contains("2 counts/cell") && text[0].contains("fail pt"),
        "{:?}",
        text[0]
    );
    assert!(text[5].starts_with("+2 more"), "{:?}", text[5]);
    let brief = table(&cores[..2], None, 60, true);
    assert!(brief[0].contains("fail"), "{:?}", brief[0]);
}

#[test]
fn core_rows_mark_the_running_workloads_top_requesters() {
    let first = WorkloadId::new(Regime::R7, 0).expect("a workload");
    let second = WorkloadId::new(Regime::R7, 1).expect("a workload");
    let status = |core_id: u8, workload, proxy| R7CoreStatus {
        core: core(core_id),
        ccd: CcdId(0),
        workload,
        top_requester: true,
        offset_proxy: proxy,
        self_sufficient: false,
        passes: 0,
    };
    let statuses = [status(0, first, false), status(1, second, true)];
    let cores = [
        core_view(0, 0, CoreState::AtLimit),
        core_view(1, 0, CoreState::AtLimit),
    ];
    let running = trial(Regime::R7, None, 120);
    let mut running = running;
    running.workload = second;
    for (width, want) in [(75, "top by offset"), (57, "top")] {
        let model = Model::new();
        let zone = TimeZone::UTC;
        let view = session(Timestamp::UNIX_EPOCH);
        let mut s = model.snapshot();
        s.session = Some(&view);
        s.cores = &cores;
        s.r7 = &statuses;
        s.trial = Some(&running);
        let scene = scene(&s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
        let rect = Rect::new(0, 0, width, 4);
        let mut buf = Buffer::empty(rect);
        let columns = if width < 60 {
            CoreColumns {
                id: 2,
                offset: 5,
                state: 9,
                gauge: 18,
                cells: 25,
                failure: 44,
                note: 50,
                brief: true,
            }
        } else {
            CoreColumns {
                id: 2,
                offset: 6,
                state: 11,
                gauge: 21,
                cells: 25,
                failure: 48,
                note: 56,
                brief: false,
            }
        };
        super::draw_ccd(
            &mut buf,
            &Ccd {
                id: CcdId(0),
                rect,
                columns,
            },
            &scene,
        );
        let rows = rows_of(&buf);
        assert!(!rows[1].contains("top"), "{:?}", rows[1]);
        assert!(rows[2].trim_end().ends_with(want), "{width}: {:?}", rows[2]);
    }
}

#[test]
fn the_top_note_fits_beside_a_note_at_its_exact_width() {
    let note = Line::from("held by C4 · solo -31");
    let width = line_width(&note) + 3 + "top requester".len();
    assert_eq!(
        plain(&top_note(&note, false, width, Colours::Full)),
        "held by C4 · solo -31 · top requester"
    );
    assert_eq!(
        plain(&top_note(&note, false, width - 1, Colours::Full)),
        "held by C4 · solo -31"
    );
}

fn ids(values: &[u8]) -> Vec<togi_machine::CoreId> {
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
        top_cell(&ids(&[0]), &ids(&[8, 9, 10, 11, 12, 13, 14, 15]), 17),
        "08-15 by offset"
    );
    assert_eq!(top_cell(&[], &[], 17), "-");
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

#[test]
fn the_requirement_line_counts_passes_and_marks_the_running_trial() {
    let model = Model::new();
    let zone = TimeZone::UTC;
    let scene = scene(
        &model.snapshot(),
        Timestamp::UNIX_EPOCH,
        &zone,
        Colours::None,
    );
    let mut t = trial(Regime::R1, None, 60);
    assert_eq!(line_width(&requirement_line(&scene, &t, false)), 0);
    t.count = Count {
        passed: 1,
        index: 2,
        of: 4,
    };
    assert_eq!(
        plain(&requirement_line(&scene, &t, false)),
        "1 passed · trial 2 of 4  ■ ► · ·"
    );
    assert_eq!(plain(&requirement_line(&scene, &t, true)), "1/4 ■ ► · ·");
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
    assert!(text.contains("screen paused until 10:10"), "{text}");
    assert!(!text.contains("ago"), "{text}");
}
