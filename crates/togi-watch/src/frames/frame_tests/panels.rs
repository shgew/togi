//! Panels whole: the history count, the combination overflow, the round, the R7 rows, the trial bar and the notes.

use jiff::tz::TimeZone;
use togi_journal::{Mce, Member, Payload};
use togi_machine::{BankType, CpuId, ObservedOffset, Regime, WorkloadId, workloads};
use togi_tuner::{CombinationId, Limit, R7CoreStatus, Reached};
use togi_watchtest::Scenario;

use super::{Fixture, SIZES, append, bar, later, main_screen, rows_of, some_time, text, words};
use crate::frames::measure::measure;
use crate::frames::scene::Scene;
use crate::model::{ComboView, CoreState, CoreView, Model, SessionView};
use crate::screen::{Colours, Journal, Screen};
use crate::tests::fixtures::core_view;
use crate::tests::kit::{Script, Start, core, nz, off, set, warning};
use crate::words::workload_display;

struct Hand {
    model: Model,
    session: SessionView,
    cores: Vec<CoreView>,
    combos: Vec<ComboView>,
}

impl Hand {
    fn new(cores: Vec<CoreView>, combos: Vec<ComboView>) -> Self {
        Self {
            model: Model::new(),
            session: SessionView {
                start: some_time(),
                activity: None,
            },
            cores,
            combos,
        }
    }

    fn journal(&self) -> Journal<'_> {
        let mut snapshot = self.model.snapshot();
        snapshot.session = Some(&self.session);
        snapshot.cores = &self.cores;
        snapshot.combos = &self.combos;
        Journal::Ready {
            snapshot,
            log: snapshot.log(),
        }
    }
}

fn region(rows: &[String], rect: ratatui::layout::Rect) -> Vec<String> {
    rows[usize::from(rect.y)..usize::from(rect.y + rect.height)]
        .iter()
        .map(|row| {
            row.chars()
                .skip(usize::from(rect.x))
                .take(usize::from(rect.width))
                .collect::<String>()
                .trim()
                .to_owned()
        })
        .collect()
}

#[test_case::test_case(120, 24; "120x24")]
#[test_case::test_case(120, 25; "120x25")]
#[test_case::test_case(160, 24; "160x24")]
#[test_case::test_case(160, 25; "160x25")]
#[test_case::test_case(240, 24; "240x24")]
#[test_case::test_case(240, 25; "240x25")]
fn a_history_too_short_for_its_entries_counts_every_one_it_leaves_out(width: u16, height: u16) {
    let cores: Vec<(u8, u8)> = (0..16).map(|id| (id, id / 8)).collect();
    let mut script = Script::new(&cores, &[Start::search(0); 16]);
    for index in 0..700 {
        script.append(warning(&format!("warning {index}")), &[]);
    }
    let fx = Fixture::of(&script.events);
    let snapshot = fx.model.snapshot();
    let total = snapshot.history.len() + snapshot.history_dropped;
    assert!(total > 700);
    let sc = main_screen(width, height);
    let zone = TimeZone::UTC;
    let scene = Scene::new(snapshot, fx.now, &zone, Colours::None);
    let history = measure(&scene, &sc).history;
    assert!(history.height >= 1, "{width}x{height}");
    let (buf, _) = fx.draw(&sc, fx.now);
    let rows = region(&rows_of(&buf), history);
    let (last, entries) = rows.split_last().expect("a history row");
    let visible = entries
        .iter()
        .filter(|row| row.as_bytes().first().is_some_and(u8::is_ascii_digit))
        .count();
    assert_eq!(
        last,
        &format!("+{} more", total - visible),
        "{width}x{height}: the last visible history row counts every hidden entry ({rows:?})"
    );
}

#[test_case::test_case(&[0, 1, 2, 3, 4, 5, 6, 7, 9], "09"; "one omitted")]
#[test_case::test_case(&[0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 15], "09 11 15"; "three omitted")]
fn a_combination_wider_than_its_columns_names_the_omitted_cores_and_the_journal_bound(
    members: &[u8],
    omitted: &str,
) {
    let cores = (0..16)
        .map(|id| {
            let mut view = core_view(id, id / 8, CoreState::AtLimit);
            view.profile = off(-20);
            view
        })
        .collect();
    let combos = vec![ComboView {
        id: CombinationId::new(nz(1)),
        members: members
            .iter()
            .rev()
            .map(|&id| Member {
                core: core(id),
                offset: off(-25),
            })
            .collect(),
        hunt: 7,
        probed: true,
        fallback: false,
        holds: None,
        clear: set(&[]),
    }];
    let hand = Hand::new(cores, combos);
    let sc = main_screen(240, 67);
    let (buf, _) = super::draw_journal(&hand.journal(), &sc, some_time(), &TimeZone::UTC);
    let Journal::Ready { snapshot, .. } = hand.journal() else {
        unreachable!()
    };
    let zone = TimeZone::UTC;
    let scene = Scene::new(snapshot, some_time(), &zone, Colours::None);
    let context = region(&rows_of(&buf), measure(&scene, &sc).context);
    assert_eq!(
        context[3],
        format!(
            "columns show 8 of {} member cores; not shown: {omitted} · l: last 400 events",
            members.len()
        ),
        "{context:?}"
    );
    for id in &members[..8] {
        assert!(context[2].contains(&format!("{id:02}")), "{}", context[2]);
    }
    for id in &members[8..] {
        assert!(!context[2].contains(&format!("{id:02}")), "{}", context[2]);
    }
}

#[test_case::test_case(160, 45, false; "medium reserves the gauge")]
#[test_case::test_case(240, 67, true; "wide retains the notes")]
fn core_notes_use_only_the_room_beyond_the_gauge(width: u16, height: u16, notes: bool) {
    let mut at_limit = core_view(0, 0, CoreState::AtLimit);
    at_limit.profile = off(-26);
    at_limit.applied = Some(ObservedOffset(-26));
    at_limit.solo = Some(off(-31));
    at_limit.holder = Some(Limit::Reached(Reached::Combination(CombinationId::new(
        nz(4),
    ))));
    let mut gave_back = core_view(8, 1, CoreState::HasRoom);
    gave_back.profile = off(-36);
    gave_back.applied = Some(ObservedOffset(-36));
    gave_back.solo = Some(off(-37));
    gave_back.gave_back = Some(CombinationId::new(nz(1)));
    let hand = Hand::new(vec![at_limit, gave_back], Vec::new());
    let (buf, _) = super::draw_journal(
        &hand.journal(),
        &main_screen(width, height),
        some_time(),
        &TimeZone::UTC,
    );
    let shown = text(&buf);
    for note in ["C4 · solo -31", "C1 · solo -37"] {
        assert_eq!(
            shown.contains(note),
            notes,
            "{width}x{height}: {note:?}:\n{shown}"
        );
    }
}

#[test]
fn the_round_keeps_its_candidate_rows_at_every_height() {
    let fx = Fixture::scenario(Scenario::Round);
    for width in [120, 160] {
        for height in 33..=60 {
            let (buf, _) = fx.main(width, height);
            let shown = text(&buf);
            assert!(
                shown.contains("gap "),
                "{width}x{height}: the round lost its candidate rows:\n{shown}"
            );
        }
    }
}

fn r7_names() -> Vec<String> {
    (0..workloads(Regime::R7).len())
        .filter_map(|index| WorkloadId::new(Regime::R7, index))
        .map(|id| {
            workload_display(id)
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

#[test_case::test_case(false, false; "own False keys False")]
#[test_case::test_case(false, true; "own False keys True")]
#[test_case::test_case(true, false; "own True keys False")]
#[test_case::test_case(true, true; "own True keys True")]
fn a_short_dashboard_reaches_every_r7_workload_and_the_running_part(own: bool, keys: bool) {
    let fx = Fixture::scenario(Scenario::Checking);
    let real = fx.model.snapshot();
    let populated: Vec<R7CoreStatus> = real
        .r7
        .iter()
        .cloned()
        .map(|mut status| {
            status.self_sufficient = status.core.get() % 2 == 0;
            status
        })
        .collect();
    let mut with_own = real;
    with_own.r7 = &populated;
    let snapshot = if own { with_own } else { real };
    let journal = Journal::Ready {
        snapshot,
        log: snapshot.log(),
    };
    let draw = |height| {
        let sc = Screen {
            keys,
            ..main_screen(120, height)
        };
        super::draw_journal(&journal, &sc, fx.now, &TimeZone::UTC).0
    };
    let buf = draw(33);
    let rows = rows_of(&buf);
    let shown = text(&buf);
    assert!(shown.contains("self-sufficient"), "keys {keys}: {shown}");
    for name in r7_names() {
        assert!(
            rows.iter().any(|row| row.starts_with(&format!(" {name} "))),
            "keys {keys}: the R7 {name} status is omitted:\n{shown}"
        );
    }
    assert!(shown.contains("trial 1 of"), "keys {keys}:\n{shown}");
    let rule = rows
        .iter()
        .position(|row| row.starts_with(" R7 ─"))
        .expect("the R7 rule");
    let more = rows[rule - 2].trim_start();
    assert!(
        more.starts_with('+') && more.contains(" more"),
        "keys {keys}: the cycle checklist is cut without saying how much it leaves out:\n{shown}"
    );
    let short = text(&draw(31));
    assert!(short.contains("trial 1 of"), "keys {keys}:\n{short}");
}

#[test]
fn a_between_trial_machine_check_is_not_hidden() {
    let mut events = Scenario::Between.events().expect("the scenario's journal");
    let boot = events.last().expect("an event").boot.to_string();
    append(
        &mut events,
        Mce {
            cpu: CpuId::from(0),
            core: Some(core(0)),
            bank: 0,
            bank_type: BankType::LoadStore,
            corrected: true,
            monotonic_ns: None,
            from_boot: None,
            trial: None,
            between_trials: true,
            lines: vec!["between-trial hardware error".to_owned()],
        }
        .into(),
        60,
        &boot,
    );
    assert!(matches!(
        events.last().map(|event| &event.payload),
        Some(Payload::Mce(_))
    ));
    let fx = Fixture::of(&events);
    let said = words(&fx.main(240, 67).0);
    assert!(
        said.contains("between trials (recorded only)"),
        "watch hides a between-trial machine check: {said}"
    );
}

#[test]
fn the_trial_bar_shows_elapsed_without_colour_at_every_size() {
    let fx = Fixture::scenario(Scenario::Search);
    for (width, height) in SIZES {
        let sc = main_screen(width, height);
        let bar_at = |now| {
            bar(&fx.draw(&sc, now).0)
                .unwrap_or_else(|| panic!("{width}x{height}: no trial bar row in the frame"))
        };
        let before = bar_at(fx.now);
        let after = bar_at(later(fx.now, 30));
        assert_ne!(
            before, after,
            "{width}x{height}: the bar is the same at two elapsed times"
        );
        let filled = |bar: &str| bar.chars().filter(|&c| c == '█').count();
        assert!(
            filled(&after) > filled(&before) && before.contains('░'),
            "{width}x{height}: elapsed cells must grow as █ over ░: {before:?} then {after:?}"
        );
    }
}

#[test]
fn a_partial_trial_is_named_by_its_ordinary_evidence() {
    let fx = Fixture::scenario(Scenario::Checking);
    let mut trial = fx.model.snapshot().trial.expect("a trial").clone();
    trial.partial = true;
    let mut snapshot = fx.model.snapshot();
    snapshot.trial = Some(&trial);
    let journal = Journal::Ready {
        snapshot,
        log: snapshot.log(),
    };
    for (width, height) in SIZES {
        let (buf, _) = super::draw_journal(
            &journal,
            &main_screen(width, height),
            fx.now,
            &TimeZone::UTC,
        );
        let shown = text(&buf);
        assert!(shown.contains("PARTIAL"), "{width}x{height}:\n{shown}");
        assert!(!shown.contains("RECORD ONLY"), "{width}x{height}:\n{shown}");
    }
}
