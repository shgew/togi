//! A held journal list: arrivals never move a row and the footer counts them.

use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;

use super::{draw_journal, some_time};
use crate::frames::layout::Layout;
use crate::frames::testing::{rows_of, screen};
use crate::history::LOG_LIMIT;
use crate::model::Model;
use crate::screen::{Colours, Journal, Log, Screen, View};
use crate::tests::kit::{Script, Start, warning};

fn warned(count: usize) -> Model {
    let mut script = Script::new(&[(0, 0)], &[Start::search(0)]);
    for index in 0..count {
        script.append(warning(&format!("warning {index}")), &[]);
    }
    script.model()
}

fn rows_in(buf: &Buffer, y: u16, height: u16) -> Vec<String> {
    rows_of(buf)[usize::from(y)..usize::from(y + height)].to_vec()
}

#[test]
fn arrivals_never_move_a_row_of_a_held_log_and_the_footer_counts_them() {
    let model = warned(LOG_LIMIT + 50);
    let snapshot = model.snapshot();
    assert!(
        snapshot.log_total > snapshot.log.len(),
        "the log is bounded"
    );
    let held = snapshot.log();
    for (width, height) in [(50, 33), (80, 33), (120, 33), (160, 45), (240, 67)] {
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
        for arrived in [1, 123] {
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
    }
}
