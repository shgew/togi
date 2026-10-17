use jiff::Timestamp;
use jiff::tz::{self, TimeZone};
use ratatui::text::Line;

use super::super::super::fit::plain;
use super::super::super::scene::Scene;
use super::{history_page, main_history};
use togi_machine::{CoreId, Regime, Signal, WorkloadId};

use crate::history::{Entry, Failed, Named, PassedIn, Passes, StepAt, Verdict, What};
use crate::model::Model;
use crate::screen::Colours;
use crate::words::{Tag, Tone};

fn at(text: &str) -> Timestamp {
    text.parse().expect("a timestamp")
}

fn minute(n: i64) -> Timestamp {
    Timestamp::from_second(1_790_000_000 + 60 * n).expect("a timestamp")
}

fn texts(lines: &[Line<'_>]) -> Vec<String> {
    lines.iter().map(plain).collect()
}

/// `entries`, newest first, as the model keeps them, drawn at `zone`.
fn frame<R>(
    entries: &[Entry],
    dropped: usize,
    zone: &TimeZone,
    draw: impl FnOnce(&Scene<'_>) -> R,
) -> R {
    let model = Model::new();
    let mut s = model.snapshot();
    s.history = entries;
    s.history_dropped = dropped;
    draw(&Scene::new(s, minute(0), zone, Colours::None))
}

fn main_rows(entries: &[Entry], width: usize, height: usize) -> Vec<String> {
    frame(entries, 0, &TimeZone::UTC, |scene| {
        texts(&main_history(scene, width, height))
    })
}

fn pass(n: i64, trial: &str, runs: u32, of: u32) -> Entry {
    let passes = Passes {
        subject: trial.to_owned(),
        runs,
        first: 1,
        of,
        peak: None,
        within: None,
    };
    Entry::new(minute(n), Tag::Pass, What::Passes(passes), Tone::Plain)
}

/// A step whose cycle's plan was not known.
const fn step_at(cycle: u32, step: u32) -> StepAt {
    StepAt {
        cycle,
        step,
        regime: None,
        workload: None,
    }
}

fn marker(n: i64, cycle: u32, step: u32) -> Entry {
    Entry::new(
        minute(n),
        Tag::Step,
        What::Step(step_at(cycle, step)),
        Tone::Plain,
    )
}

/// `entry` as passed in the step `at`, the way a trial that ran in a checking step records it.
fn in_step(mut entry: Entry, at: StepAt) -> Entry {
    if let What::Passes(passes) = &mut entry.what {
        passes.within = Some(PassedIn::Step(at));
    }
    entry
}

fn plain_entry(n: i64, tag: Tag, text: &str, tone: Tone) -> Entry {
    Entry::line(minute(n), tag, text.to_owned(), tone)
}

fn failure(n: i64, subject: &str) -> Entry {
    let failed = Failed {
        subject: subject.to_owned(),
        verdict: Verdict {
            signal: Some(Signal::ComputationError),
            when: Some("early in trial 2 of 5".to_owned()),
            named: Some(Named::Core(CoreId::new(4).expect("a core id"))),
        },
        hunt: None,
        reboot: None,
    };
    Entry::new(minute(n), Tag::Fail, What::Failed(failed), Tone::Bad)
}

/// Entries newest first: the last one built is the newest.
fn newest_first(mut oldest_first: Vec<Entry>) -> Vec<Entry> {
    oldest_first.reverse();
    oldest_first
}

#[test]
fn a_recent_failure_stays_visible_under_many_routine_passes() {
    let mut story = vec![failure(0, "R3 all-core on 00-15 at -30")];
    for n in 1..=40 {
        let step = u32::try_from(n).expect("small");
        story.push(marker(n, 3, step));
        story.push(in_step(
            pass(n, &format!("core {:02} R1 light", n % 16), 1, 1),
            step_at(3, step),
        ));
    }
    let rows = main_rows(&newest_first(story), 100, 4);
    assert!(rows.len() <= 4, "{rows:?}");
    assert!(
        rows.iter()
            .any(|row| row.contains("wrong result") && row.contains("core 04 named")),
        "{rows:?}"
    );
    assert_eq!(
        rows.len(),
        4,
        "three folded step summaries, then the failure: {rows:?}"
    );
    for (row, step) in rows.iter().zip([40, 39, 38]) {
        assert!(
            row.contains(&format!("cycle 3 step {step} ")) && row.contains("1 credited check"),
            "{rows:?}"
        );
    }
}

#[test]
fn the_newest_failure_replaces_the_oldest_row_when_newer_important_entries_fill_the_rows() {
    let mut story = vec![failure(0, "R7 on 00-15")];
    for n in 1..=8 {
        story.push(plain_entry(
            n,
            Tag::Limit,
            &format!("core {n:02} solo limit -20"),
            Tone::Good,
        ));
    }
    let rows = main_rows(&newest_first(story), 100, 4);
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert!(rows[3].contains("wrong result"), "{rows:?}");
    assert!(rows[0].contains("core 08"), "{rows:?}");
}

#[test]
fn routine_passes_fold_into_credited_checks_and_surplus_passes_do_not_add_up() {
    let story = vec![
        marker(0, 2, 5),
        in_step(pass(1, "R3 all-core part 1", 5, 3), step_at(2, 5)),
        in_step(pass(2, "R3 all-core part 2", 2, 2), step_at(2, 5)),
    ];
    let rows = main_rows(&newest_first(story), 120, 4);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("cycle 2 step 5"), "{rows:?}");
    assert!(
        rows[0].contains("5 credited checks"),
        "3 + 2, not 5 + 2: {rows:?}"
    );
}

#[test]
fn consecutive_successful_steps_fold_into_separately_attributed_summaries() {
    let story = vec![
        marker(0, 2, 4),
        in_step(pass(1, "R3 all-core part 1", 3, 3), step_at(2, 4)),
        marker(2, 2, 5),
        in_step(pass(3, "R7 all-core part 1", 2, 2), step_at(2, 5)),
    ];
    let rows = main_rows(&newest_first(story), 120, 4);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[0].contains("cycle 2 step 5")
            && rows[0].contains("2 credited checks")
            && !rows[0].contains("step 4"),
        "{rows:?}"
    );
    assert!(
        rows[1].contains("cycle 2 step 4") && rows[1].contains("3 credited checks"),
        "{rows:?}"
    );
}

#[test]
fn passes_of_a_step_without_a_marker_fold_apart_under_their_own_step() {
    let r2 = "cycle 1 step 4: R2 heavy vector, y-cruncher FFTv4 + N63 + VT3";
    let step4 = StepAt {
        regime: Some(Regime::R2),
        workload: WorkloadId::by_id("ycruncher-fftv4-n63-vt3"),
        ..step_at(1, 4)
    };
    let story = vec![
        marker(0, 1, 3),
        in_step(pass(1, "R7 all-core part 1", 4, 4), step_at(1, 3)),
        in_step(pass(2, "R2 heavy vector on core 15", 2, 2), step4),
        in_step(pass(3, "R2 heavy vector on core 14", 3, 3), step4),
        pass(4, "core 00 R1 light", 1, 1),
    ];
    let rows = main_rows(&newest_first(story), 140, 6);
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(
        rows[0].contains("1 credited check") && !rows[0].contains("step"),
        "a pass outside any step stays its own summary: {rows:?}"
    );
    assert!(
        rows[1].contains(r2) && rows[1].contains("5 credited checks"),
        "{rows:?}"
    );
    assert!(
        rows[2].contains("cycle 1 step 3")
            && rows[2].contains("4 credited checks")
            && !rows[2].contains("R2"),
        "{rows:?}"
    );
}

#[test]
fn a_lone_chain_note_keeps_its_explanation_when_folded() {
    let note = "cycle 2 step 5 · CCD 1 partial 2 loads 04-07";
    let rows = main_rows(&[plain_entry(0, Tag::Chain, note, Tone::Plain)], 120, 4);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains(note), "{rows:?}");
    assert!(!rows[0].contains("chain note"), "{rows:?}");
}

#[test]
fn a_chain_derivation_burst_and_a_round_completion_each_collapse_to_one_row() {
    let story = vec![
        plain_entry(
            0,
            Tag::Chain,
            "cycle 2 step 5 · CCD 0 partials end",
            Tone::Plain,
        ),
        plain_entry(
            1,
            Tag::Chain,
            "cycle 2 step 5 · CCD 1 partials end",
            Tone::Plain,
        ),
        plain_entry(
            2,
            Tag::Chain,
            "cycle 2 step 5 · partial 3 loads 00-03",
            Tone::Plain,
        ),
    ];
    let rows = main_rows(&newest_first(story), 120, 4);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("3 chain notes"), "{rows:?}");

    let story = vec![
        pass(0, "core 00 R1 light", 3, 3),
        pass(1, "core 01 R1 light", 3, 3),
        plain_entry(
            2,
            Tag::Round,
            "#3 passed, the proposed offsets held",
            Tone::Good,
        ),
    ];
    let rows = main_rows(&newest_first(story), 120, 4);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains("#3 passed, the proposed offsets held"),
        "{rows:?}"
    );
    assert!(rows[0].contains("6 credited checks"), "{rows:?}");
}

#[test]
fn failures_decisions_lifecycle_and_resets_are_never_folded_away() {
    let story = vec![
        pass(0, "core 00 R1 light", 1, 1),
        plain_entry(1, Tag::Crash, "the machine rebooted", Tone::Bad),
        plain_entry(
            2,
            Tag::Step,
            "core 03 passed -10; searches -15 on its next turn",
            Tone::Plain,
        ),
        plain_entry(3, Tag::Reset, "core 05 reset to search", Tone::Warn),
        plain_entry(4, Tag::Stop, "stopped by a signal", Tone::Plain),
        plain_entry(5, Tag::Round, "#4: core 02 -5", Tone::Plain),
        plain_entry(
            6,
            Tag::Unclear,
            "attribution unclear: no core named",
            Tone::Warn,
        ),
    ];
    let rows = main_rows(&newest_first(story), 120, 12);
    assert_eq!(rows.len(), 7, "{rows:?}");
    for needle in [
        "the machine rebooted",
        "searches -15",
        "core 05 reset",
        "stopped by a signal",
        "#4: core 02 -5",
        "no core named",
    ] {
        assert!(
            rows.iter().any(|row| row.contains(needle)),
            "{needle}: {rows:?}"
        );
    }
}

#[test]
fn an_important_entry_takes_a_second_row_only_when_one_does_not_fit() {
    let long = failure(0, "R3 all-core on 00-15 at -30 after the long wait");
    let rows = main_rows(std::slice::from_ref(&long), 60, 4);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[1].starts_with("                "),
        "indented under the sentence: {rows:?}"
    );
    let rows = main_rows(&[long], 160, 4);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let rows = main_rows(&[plain_entry(0, Tag::Note, "x", Tone::Plain)], 60, 4);
    assert_eq!(rows.len(), 1);
}

#[test]
fn main_history_never_exceeds_its_rows_or_its_width() {
    let mut story = Vec::new();
    for n in 0..30 {
        story.push(failure(n, "R3 all-core on 00-15 at -30"));
        story.push(pass(n, "core 00 R1 light", 1, 1));
    }
    let story = newest_first(story);
    for width in [20, 40, 60, 100] {
        for height in 1..6 {
            let rows = main_rows(&story, width, height);
            assert!(rows.len() <= height, "{width}x{height}: {rows:?}");
            for row in &rows {
                assert!(row.chars().count() <= width, "{width}x{height}: {row:?}");
            }
        }
    }
    assert!(main_rows(&story, 0, 4).is_empty());
    assert!(main_rows(&story, 80, 0).is_empty());
}

#[test]
fn nothing_to_show_is_said_in_words() {
    assert_eq!(main_rows(&[], 80, 4), ["nothing has happened yet"]);
    let page = frame(&[], 0, &TimeZone::UTC, |scene| {
        texts(&history_page(scene, 80))
    });
    assert_eq!(page, ["nothing has happened yet; page 8 lists every event"]);
}

#[test]
fn the_history_page_lists_every_entry_newest_first_under_local_date_separators() {
    let story = newest_first(vec![
        Entry::line(
            at("2026-10-17T22:30:00Z"),
            Tag::Start,
            "session started on 16 cores".to_owned(),
            Tone::Plain,
        ),
        Entry::line(
            at("2026-10-17T23:45:00Z"),
            Tag::Limit,
            "core 00 solo limit -20".to_owned(),
            Tone::Good,
        ),
        Entry::line(
            at("2026-10-18T00:15:00Z"),
            Tag::Fail,
            "R3 all-core".to_owned(),
            Tone::Bad,
        ),
    ]);
    let utc = frame(&story, 0, &TimeZone::UTC, |scene| {
        texts(&history_page(scene, 70))
    });
    let dates: Vec<&String> = utc.iter().filter(|row| row.starts_with("20")).collect();
    assert_eq!(dates.len(), 2, "{utc:?}");
    assert!(utc[0].starts_with("2026-10-18"), "{utc:?}");
    assert!(
        utc[1].contains("00:15") && utc[1].contains("fail"),
        "{utc:?}"
    );
    assert!(utc[2].starts_with("2026-10-17"), "{utc:?}");
    assert_eq!(utc.len(), 5, "{utc:?}");

    let tokyo = TimeZone::fixed(tz::offset(9));
    let local = frame(&story, 0, &tokyo, |scene| texts(&history_page(scene, 70)));
    let separators: Vec<&String> = local.iter().filter(|row| row.starts_with("20")).collect();
    assert_eq!(
        separators.len(),
        1,
        "all three fall on 2026-10-18 at +09:00: {local:?}"
    );
    assert!(local[0].starts_with("2026-10-18"), "{local:?}");
    assert_eq!(local.len(), 4);
}

#[test]
fn the_history_page_keeps_routine_entries_unfolded_and_wraps_long_ones() {
    let mut story = vec![failure(
        0,
        "R7 all-core on 00-15 at -30 after the long wait",
    )];
    for n in 1..=5 {
        story.push(pass(n, &format!("core {n:02} R1 light"), 1, 1));
    }
    let page = frame(&newest_first(story), 0, &TimeZone::UTC, |scene| {
        texts(&history_page(scene, 50))
    });
    assert_eq!(
        page.iter().filter(|row| row.contains(" pass ")).count(),
        5,
        "{page:?}"
    );
    assert!(page.len() > 1 + 5 + 1, "the failure wraps: {page:?}");
    assert!(page.iter().all(|row| row.chars().count() <= 50), "{page:?}");
}

#[test]
fn the_history_page_counts_what_the_model_no_longer_keeps() {
    let story = [plain_entry(0, Tag::Note, "kept", Tone::Plain)];
    let page = frame(&story, 12, &TimeZone::UTC, |scene| {
        texts(&history_page(scene, 90))
    });
    assert_eq!(
        page.last().map(String::as_str),
        Some("12 older entries are not kept here; page 8 lists every event")
    );
}

#[test]
fn a_sentence_that_is_no_list_is_cut_to_whole_words_not_counted() {
    let start = plain_entry(
        0,
        Tag::Hunt,
        "#4 started · part 1: 00 at failing offsets, 01 02 parked",
        Tone::Warn,
    );
    let rows = main_rows(&[start], 71, 1);
    assert_eq!(
        rows[0].get(16..),
        Some("#4 started · part 1: 00 at failing offsets, 01 02..."),
        "{rows:?}"
    );
}

#[test]
fn a_round_that_lists_many_cores_keeps_whole_items_and_counts_the_rest() {
    let round = Entry::new(
        minute(1),
        Tag::Round,
        What::List {
            head: "round #1".to_owned(),
            items: [
                "core 00 goes deeper to -26",
                "core 06 goes deeper to -45",
                "core 08 goes deeper to -35",
                "core 09 goes deeper to -45",
                "core 11 goes deeper to -41",
            ]
            .map(str::to_owned)
            .to_vec(),
        },
        Tone::Plain,
    );
    for width in [80, 100, 120] {
        let rows = main_rows(std::slice::from_ref(&round), width, 3);
        let joined = rows.join(" ");
        assert!(
            !joined.contains("..") && !joined.contains('…'),
            "{width}: {rows:?}"
        );
        assert!(
            joined.contains("core 00 goes deeper to -26"),
            "{width}: {rows:?}"
        );
        assert!(
            joined.contains("-41") || joined.contains(" more"),
            "{width}: the cores left out are counted: {rows:?}"
        );
    }
}
