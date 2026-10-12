//! Plain-text layout for `status`: word wrapping and aligned tables that fit 80 columns.

use std::fmt::Write as _;

/// The width in characters `status` fits; a word wider than the room overflows rather than split.
const WIDTH: usize = 80;

fn width(text: &str) -> usize {
    text.chars().count()
}

/// Appends `text` wrapped to [`WIDTH`]: the first line starts with `first`, the rest with `rest`.
/// Leading newlines of `first` become blank lines before the text.
pub(crate) fn wrap(out: &mut String, first: &str, rest: &str, text: &str) {
    let body = first.trim_start_matches('\n');
    out.push_str(&first[..first.len() - body.len()]);
    let mut line = body.to_owned();
    let mut used = width(body);
    let mut empty = true;
    for word in text.split_whitespace() {
        let n = width(word);
        if !empty && used + 1 + n > WIDTH {
            out.push_str(&line);
            out.push('\n');
            rest.clone_into(&mut line);
            used = width(rest);
            empty = true;
        }
        if !empty {
            line.push(' ');
            used += 1;
        }
        line.push_str(word);
        used += n;
        empty = false;
    }
    out.push_str(&line);
    out.push('\n');
}

/// A table row: its cells, and notes printed wrapped under it so their length cannot widen the table.
pub(crate) struct Row {
    cells: Vec<String>,
    notes: Vec<(&'static str, String)>,
}

impl Row {
    pub(crate) fn new<const N: usize>(cells: [String; N]) -> Self {
        Self {
            cells: cells.into(),
            notes: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) fn note(mut self, label: &'static str, text: String) -> Self {
        self.notes.push((label, text));
        self
    }
}

/// Appends `rows` as a table, columns separated by two spaces and the last one unpadded, each row
/// followed by its notes. A row wider than [`WIDTH`] is wrapped.
pub(crate) fn table(out: &mut String, rows: &[Row]) {
    let columns = rows.iter().map(|row| row.cells.len()).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.cells.get(column))
                .map(|cell| width(cell))
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in rows {
        let mut line = String::new();
        for (column, cell) in row.cells.iter().enumerate() {
            if column + 1 == row.cells.len() {
                line.push_str(cell);
            } else {
                let _ = write!(line, "{cell:<pad$}  ", pad = widths[column]);
            }
        }
        if width(&line) > WIDTH {
            wrap(out, "", "  ", &line);
        } else {
            out.push_str(&line);
            out.push('\n');
        }
        for (label, text) in &row.notes {
            let prefix = format!("  {label} ");
            let indent = " ".repeat(width(&prefix));
            wrap(out, &prefix, &indent, text);
        }
    }
}
