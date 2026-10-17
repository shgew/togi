//! Scrolling a long body inside a rectangle, with a scrollbar thumb.
//!
//! A scroll at or past the extent shows the end, so `usize::MAX` follows it.

use ratatui::text::{Line, Span};

use super::colours::Paint;
use super::fit::truncate_styled;
use crate::screen::Colours;

/// The scrollbar column: a track of `page` rows with a thumb as tall as the page is of the whole, placed where
/// `top` is of `scrolled`. A page that needs no scrolling shows no bar.
#[must_use]
pub fn scrollbar(
    page: usize,
    total: usize,
    top: usize,
    scrolled: usize,
    colours: Colours,
) -> Vec<Span<'static>> {
    if scrolled == 0 || total == 0 {
        return vec![Span::raw(" "); page];
    }
    let thumb = (page * page / total).max(1).min(page);
    let start = top * (page - thumb) / scrolled.max(1);
    (0..page)
        .map(|row| {
            if (start..start + thumb).contains(&row) {
                Paint::Text.span(colours, "█")
            } else {
                Paint::Grey.span(colours, "│")
            }
        })
        .collect()
}

/// `lines` as a page of `height` rows scrolled `scroll` lines down, each `width` cells wide with the scrollbar
/// in its first two. Returns the page and how far the lines can scroll.
#[must_use]
pub fn scroll_body_with_bar(
    lines: &[Line<'static>],
    width: usize,
    height: usize,
    scroll: usize,
    colours: Colours,
) -> (Vec<Line<'static>>, usize) {
    let scrolled = lines.len().saturating_sub(height);
    let top = scroll.min(scrolled);
    let bar = scrollbar(height, lines.len(), top, scrolled, colours);
    let body = &lines[top..lines.len().min(top + height)];
    let page = body
        .iter()
        .zip(bar)
        .map(|(line, bar)| {
            let mut row = vec![bar, Span::raw(" ")];
            row.extend(truncate_styled(line, width.saturating_sub(2)).spans);
            truncate_styled(&Line::from(row), width)
        })
        .collect();
    (page, scrolled)
}

#[cfg(test)]
mod tests {
    use ratatui::text::Line;
    use test_case::test_case;

    use super::{scroll_body_with_bar, scrollbar};
    use crate::frames::fit::plain;
    use crate::screen::Colours;

    #[test_case(4, 3, 0, 0, "    "; "a page that fits shows no bar")]
    #[test_case(10, 40, 0, 30, "██││││││││"; "the top puts the thumb at the top")]
    #[test_case(10, 40, 30, 30, "││││││││██"; "the end puts the thumb at the bottom")]
    #[test_case(10, 40, 15, 30, "││││██││││"; "halfway puts it in the middle")]
    #[test_case(10, 1000, 990, 990, "│││││││││█"; "a long view keeps a one-row thumb")]
    fn the_scrollbar_thumb_tracks_the_page(
        page: usize,
        total: usize,
        top: usize,
        scrolled: usize,
        want: &str,
    ) {
        let bar: String = scrollbar(page, total, top, scrolled, Colours::Full)
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(bar, want);
    }

    #[test]
    fn a_zero_height_page_has_no_bar() {
        assert!(scrollbar(0, 40, 0, 30, Colours::Full).is_empty());
    }

    fn numbered(count: usize) -> Vec<Line<'static>> {
        (0..count)
            .map(|n| Line::from(format!("row {n:02}")))
            .collect()
    }

    #[test_case(20; "at the extent")]
    #[test_case(21; "one past the extent")]
    #[test_case(1000; "far past the extent")]
    fn a_scroll_at_or_past_the_extent_shows_the_end(scroll: usize) {
        let lines = numbered(30);
        let (end, extent) = scroll_body_with_bar(&lines, 20, 10, usize::MAX, Colours::None);
        assert_eq!(extent, 20);
        assert_eq!(
            scroll_body_with_bar(&lines, 20, 10, scroll, Colours::None).0,
            end
        );
        assert_eq!(plain(&end[9]), "█ row 29");
    }

    #[test]
    fn the_top_and_a_middle_scroll_put_the_thumb_where_they_are() {
        let lines = numbered(30);
        let (top, _) = scroll_body_with_bar(&lines, 20, 10, 0, Colours::None);
        assert_eq!(plain(&top[0]), "█ row 00");
        let (middle, _) = scroll_body_with_bar(&lines, 20, 10, 5, Colours::None);
        assert_eq!(plain(&middle[0]), "│ row 05");
    }

    #[test]
    fn a_body_that_fits_has_blank_bar_columns_and_rows_clipped_to_the_width() {
        let (page, extent) = scroll_body_with_bar(&numbered(3), 5, 10, 0, Colours::None);
        assert_eq!(extent, 0);
        assert_eq!(
            page.iter().map(plain).collect::<Vec<_>>(),
            ["  row", "  row", "  row"]
        );
        assert!(
            scroll_body_with_bar(&numbered(3), 5, 0, 0, Colours::None)
                .0
                .is_empty()
        );
        assert_eq!(
            plain(&scroll_body_with_bar(&numbered(3), 1, 3, 0, Colours::None).0[0]),
            " "
        );
    }
}
