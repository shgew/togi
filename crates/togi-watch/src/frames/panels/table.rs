//! The rows every context table shares: a row of columns, a label/value row, the stage markers and the label
//! written into a row's gutter.

use std::borrow::Cow;

use ratatui::text::{Line, Span};

use super::super::colours::Paint;
use super::super::fit::{cut_styled, line_width, plain};
use super::super::measure::{Column, Tables};
use super::super::scene::Scene;

/// Each value in its column. A value runs on through the columns after it while they are empty, so a row that
/// leaves a column blank can use its cells; a column of width 0 is skipped.
pub(super) fn table_row(cols: &[Column], values: &[Line<'static>]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut x = 0;
    for (i, col) in cols.iter().enumerate() {
        let Some(value) = values.get(i) else { break };
        if col.w == 0 || line_width(value) == 0 || col.at < x {
            continue;
        }
        let mut width = col.w;
        for (j, next) in cols.iter().enumerate().skip(i + 1) {
            if next.w == 0 {
                continue;
            }
            if values.get(j).is_some_and(|value| line_width(value) > 0) {
                break;
            }
            width = next.at + next.w - col.at;
        }
        let value = cut_styled(value, width);
        if col.at > x {
            spans.push(Span::raw(" ".repeat(col.at - x)));
        }
        x = col.at + line_width(&value);
        spans.extend(value.spans);
    }
    Line::from(spans)
}

/// A label/value row: the label in grey, the value starting at the tables' value column, in `width` cells.
pub(super) fn field_row(
    scene: &Scene<'_>,
    t: &Tables,
    label: &str,
    value: Line<'static>,
    width: usize,
) -> Line<'static> {
    let head = if label.is_empty() {
        Span::raw(" ".repeat(t.field))
    } else {
        scene.paint(Paint::Grey, format!("{label:<width$}", width = t.field))
    };
    let mut spans = vec![head];
    spans.extend(value.spans);
    cut_styled(&Line::from(spans), width)
}

/// A stage marker: `■` done in green, `►` running, anything else on the track.
pub(super) fn stage_marker(scene: &Scene<'_>, mark: &'static str) -> Span<'static> {
    let paint = match mark {
        "■" => Paint::Good,
        "►" => Paint::Running,
        _ => Paint::Track,
    };
    scene.paint(paint, mark)
}

/// `label` written over the start of a row that leaves those cells blank; a row without that gutter keeps its
/// text.
pub(super) fn overlay(label: Span<'static>, row: Line<'static>) -> Line<'static> {
    let n = label.width();
    if n == 0 {
        return row;
    }
    if line_width(&row) <= n {
        return Line::from(label);
    }
    if !plain(&row).chars().take(n + 1).all(char::is_whitespace) {
        return row;
    }
    let mut spans = vec![label];
    let mut skip = n;
    for span in row.spans {
        if skip == 0 {
            spans.push(span);
            continue;
        }
        let chars = span.content.chars().count();
        if chars <= skip {
            skip -= chars;
            continue;
        }
        let rest: String = span.content.chars().skip(skip).collect();
        skip = 0;
        spans.push(Span::styled(rest, span.style));
    }
    Line::from(spans)
}

/// Numbers written compactly: a run of consecutive numbers becomes a range, `1-3, 5`.
pub(super) fn number_ranges(values: &[u32]) -> String {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i;
        while j + 1 < sorted.len() && sorted[j + 1] == sorted[j] + 1 {
            j += 1;
        }
        if j > i {
            parts.push(format!("{}-{}", sorted[i], sorted[j]));
        } else {
            parts.push(sorted[i].to_string());
        }
        i = j + 1;
    }
    parts.join(", ")
}

/// `text` as one cell in `paint`.
pub(super) fn cell(
    scene: &Scene<'_>,
    paint: Paint,
    text: impl Into<Cow<'static, str>>,
) -> Line<'static> {
    Line::from(scene.paint(paint, text))
}

/// Whether a row shows nothing but blanks.
pub(super) fn is_blank(line: &Line<'_>) -> bool {
    plain(line).trim().is_empty()
}

#[cfg(test)]
mod tests {
    use ratatui::text::{Line, Span};

    use super::super::super::fit::plain;
    use super::super::super::measure::Column;
    use super::{number_ranges, overlay, table_row};

    fn col(at: usize, w: usize) -> Column {
        Column { at, w }
    }

    #[test]
    fn a_value_runs_on_through_the_empty_columns_after_it() {
        let cols = [col(0, 3), col(4, 3), col(8, 10)];
        let row = table_row(
            &cols,
            &[Line::from("a long value"), Line::default(), Line::from("x")],
        );
        assert_eq!(plain(&row), "a...    x");
        let row = table_row(
            &cols,
            &[Line::from("a long value"), Line::default(), Line::default()],
        );
        assert_eq!(plain(&row), "a long value");
    }

    #[test]
    fn a_column_of_width_zero_is_skipped() {
        let cols = [col(0, 2), col(3, 0), col(3, 5)];
        let row = table_row(
            &cols,
            &[Line::from("ab"), Line::from("hidden"), Line::from("cd")],
        );
        assert_eq!(plain(&row), "ab cd");
    }

    #[test]
    fn a_label_takes_a_blank_gutter_and_leaves_text_alone() {
        let row = Line::from("          group 1");
        assert_eq!(
            plain(&overlay(Span::raw("parts"), row)),
            "parts     group 1"
        );
        let full = Line::from("group 1 at -20");
        assert_eq!(
            plain(&overlay(Span::raw("parts"), full.clone())),
            plain(&full)
        );
        assert_eq!(
            plain(&overlay(Span::raw("parts"), Line::from("ab"))),
            "parts"
        );
        assert_eq!(plain(&overlay(Span::raw(""), full.clone())), plain(&full));
    }

    #[test]
    fn consecutive_numbers_become_ranges() {
        assert_eq!(number_ranges(&[]), "");
        assert_eq!(number_ranges(&[5, 1, 2, 3, 9, 10]), "1-3, 5, 9-10");
        assert_eq!(number_ranges(&[4]), "4");
    }
}
