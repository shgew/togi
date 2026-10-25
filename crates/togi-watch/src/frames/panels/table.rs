//! The rows the context panel shares: a label/value row and a one-cell line.

use std::borrow::Cow;

use ratatui::text::{Line, Span};

use super::super::colours::Paint;
use super::super::fit::cut_styled;
use super::super::scene::Scene;

/// The cells a label/value row gives its label.
const FIELD: usize = 12;

/// A label/value row: the label in grey, the value starting at the value column, in `width` cells. An empty
/// label leaves the column blank so a value can continue the row above it.
pub(super) fn field_row(
    scene: &Scene<'_>,
    label: &str,
    value: Line<'static>,
    width: usize,
) -> Line<'static> {
    let head = if label.is_empty() {
        Span::raw(" ".repeat(FIELD))
    } else {
        scene.paint(Paint::Grey, format!("{label:<FIELD$}"))
    };
    let mut spans = vec![head];
    spans.extend(value.spans);
    cut_styled(&Line::from(spans), width, scene.colours)
}

/// `text` as one cell in `paint`.
pub(super) fn cell(
    scene: &Scene<'_>,
    paint: Paint,
    text: impl Into<Cow<'static, str>>,
) -> Line<'static> {
    Line::from(scene.paint(paint, text))
}

#[cfg(test)]
mod tests {
    use ratatui::text::Line;

    use super::super::super::fit::plain;
    use super::super::super::scene::Scene;
    use super::field_row;
    use crate::model::Model;
    use crate::screen::Colours;

    #[test]
    fn a_label_gets_its_column_and_the_value_follows() {
        let model = Model::new();
        let snapshot = model.snapshot();
        let zone = jiff::tz::TimeZone::UTC;
        let scene = Scene::new(snapshot, jiff::Timestamp::UNIX_EPOCH, &zone, Colours::None);
        let row = field_row(&scene, "hold", Line::from("12h of 50h"), 40);
        assert_eq!(plain(&row, Colours::None), "hold        12h of 50h");
        let row = field_row(&scene, "", Line::from("more"), 40);
        assert_eq!(plain(&row, Colours::None), "            more");
        let row = field_row(&scene, "hold", Line::from("12h of 50h"), 18);
        assert_eq!(plain(&row, Colours::None), "hold        12h...");
    }
}
