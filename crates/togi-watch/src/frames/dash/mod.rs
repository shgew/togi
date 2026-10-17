//! The dashboard's own rows: the header, the stage line, Main's phase progress and NOW rows, the CCD gauges and
//! core tables, and the R7 lines.

mod ccd;
mod header;
mod now;
mod r7;
mod stage;

pub(crate) use ccd::{core_table, cores_page, draw_core_overview, note_line};
pub(crate) use header::{header, status_header};
pub(crate) use now::now_lines;
pub(crate) use r7::r7_lines;
pub(crate) use stage::{phase_progress, stage_line};

fn at(cells: usize) -> u16 {
    u16::try_from(cells).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests;
