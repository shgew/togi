//! The dashboard's own rows: the header, the stage line, the trial band with its bar, the CCD tables with their
//! gauges.

mod ccd;
mod header;
mod now;
mod stage;

pub(crate) use ccd::{draw_ccd, marks};
pub(crate) use header::{header, status_header};
pub(crate) use now::draw_now;
pub(crate) use stage::stage_line;

#[cfg(test)]
mod tests;
