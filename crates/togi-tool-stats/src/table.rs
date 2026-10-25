use std::fmt::Write as _;

/// Report sections as aligned text tables: a title, a header and rows, a blank line between sections. A
/// cell is padded to the widest cell of its column in the section plus two spaces, except a row's last cell.
#[derive(Default)]
pub(crate) struct Table {
    text: String,
    started: bool,
    rows: Vec<Vec<String>>,
}

macro_rules! row {
    ($table:expr, $($cell:expr),+ $(,)?) => {
        $table.row(vec![$($cell.to_string()),+])
    };
}
pub(crate) use row;

impl Table {
    pub(crate) fn section(&mut self, title: &str, header: &[&str]) {
        self.flush();
        if self.started {
            self.text.push('\n');
        }
        self.started = true;
        self.text.push_str(title);
        self.text.push('\n');
        self.rows
            .push(header.iter().map(|&cell| cell.to_owned()).collect());
    }

    pub(crate) fn row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
    }

    pub(crate) fn finish(mut self) -> String {
        self.flush();
        self.text
    }

    fn flush(&mut self) {
        let width = |column: usize| {
            self.rows
                .iter()
                .filter(|row| column + 1 < row.len())
                .map(|row| row[column].chars().count())
                .max()
        };
        let widths: Vec<usize> = (0..self.rows.iter().map(Vec::len).max().unwrap_or(0))
            .map(|column| width(column).unwrap_or(0))
            .collect();
        for row in self.rows.drain(..) {
            for (column, cell) in row.iter().enumerate() {
                if column + 1 < row.len() {
                    let _ = write!(self.text, "{cell:<0$}", widths[column] + 2);
                } else {
                    self.text.push_str(cell);
                }
            }
            self.text.push('\n');
        }
    }
}
