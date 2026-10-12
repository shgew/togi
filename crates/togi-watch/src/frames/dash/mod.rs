//! The dashboard's own rows: the header, the stage line, the trial band with its bar, the CCD tables with their
//! gauges, and the R7 lines.

mod ccd;
mod header;
mod now;
mod r7;
mod stage;

pub(crate) use ccd::{draw_ccd, note_line};
pub(crate) use header::{header, status_header};
pub(crate) use now::draw_now;
pub(crate) use r7::r7_lines;
pub(crate) use stage::stage_line;

fn at(cells: usize) -> u16 {
    u16::try_from(cells).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests;
