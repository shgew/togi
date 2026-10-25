//! A held journal list: arrivals never move a row and the footer counts them.

use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;

use super::{draw_journal, journal, some_time};
use crate::frames::layout::Layout;
use crate::frames::testing::{rows_of, screen};
use crate::history::LOG_LIMIT;
use crate::model::Model;
use crate::screen::{Colours, Journal, Log, Screen, View};
use crate::tests::kit::warning;

fn warned(count: usize) -> Model {
    let warnings = (0..count)
        .map(|index| warning(&format!("warning {index}")))
        .collect();
    crate::model::project(&journal(warnings))
}

fn rows_in(buf: &Buffer, y: u16, height: u16) -> Vec<String> {
    rows_of(buf)[usize::from(y)..usize::from(y + height)].to_vec()
}

#[test_case::test_case(50, 33, 1; "50x33 arrived 1")]
#[test_case::test_case(50, 33, 123; "50x33 arrived 123")]
#[test_case::test_case(80, 33, 1; "80x33 arrived 1")]
#[test_case::test_case(80, 33, 123; "80x33 arrived 123")]
#[test_case::test_case(120, 33, 1; "120x33 arrived 1")]
#[test_case::test_case(120, 33, 123; "120x33 arrived 123")]
#[test_case::test_case(160, 45, 1; "160x45 arrived 1")]
#[test_case::test_case(160, 45, 123; "160x45 arrived 123")]
#[test_case::test_case(240, 67, 1; "240x67 arrived 1")]
#[test_case::test_case(240, 67, 123; "240x67 arrived 123")]
fn arrivals_never_move_a_row_of_a_held_log_and_the_footer_counts_them(
    width: u16,
    height: u16,
    arrived: usize,
) {
    let model = warned(LOG_LIMIT + 50);
    let snapshot = model.snapshot();
    assert!(
        snapshot.log_total > snapshot.log.len(),
        "the log is bounded"
    );
    let held = snapshot.log();
    let sc = Screen {
        view: View::Log,
        ..screen(View::Log, width, height, Colours::None, true)
    };
    let layout = Layout::new(&sc);
    let frame = |arrived: usize| {
        let journal = Journal::Ready {
            snapshot,
            log: Log { arrived, ..held },
        };
        draw_journal(&journal, &sc, some_time(), &TimeZone::UTC)
    };
    let (held_buf, held_drawn) = frame(0);
    let footer = rows_of(&held_buf)[usize::from(layout.footer.y)].clone();
    assert!(
        !footer.contains("new event"),
        "{width}x{height}: a fresh hold has an arrival notice: {footer:?}"
    );
    let body = rows_in(&held_buf, layout.body.y, layout.body.height);
    let (buf, drawn) = frame(arrived);
    assert_eq!(
        rows_in(&buf, layout.body.y, layout.body.height),
        body,
        "{width}x{height}: {arrived} arrivals move the held list"
    );
    assert_eq!(
        drawn.scroll, held_drawn.scroll,
        "{width}x{height}: {arrived} arrivals move the held scroll"
    );
    let count = if arrived == 1 {
        "1 new event".to_owned()
    } else {
        format!("{arrived} new events")
    };
    let footer = rows_of(&buf)[usize::from(layout.footer.y)].clone();
    assert!(
        footer.contains(&format!("{count} · End: latest")),
        "{width}x{height}: the arrival count and the way back are not visible: {footer:?}"
    );
}
