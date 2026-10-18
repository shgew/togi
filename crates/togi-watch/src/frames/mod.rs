//! The dashboard frames, on ratatui buffers.
//!
//! - [`render`]: one frame of the journal on a screen, and how long it stays true.
//! - [`Paint`] and [`colours_for`]: the drawing roles the panels paint with and
//!   how each [`Colours`](crate::screen::Colours) profile applies them.
//! - [`cut_words`], [`fit_clauses`], [`CoreNote`] and the rest of the fitting:
//!   every line is measured as the console shows it, escaped, and never cut
//!   inside a value or a word.
//! - [`Layout`] and [`check_frame_bounds`]: the size classes and the frame area,
//!   `(width - 1) x (height - 1)`.
//! - [`help_body`] and [`footer`]: the help view and the key hints.

mod colours;
#[cfg(test)]
pub(crate) mod corpus;
mod dash;
mod draw;
mod fit;
mod footer;
#[cfg(test)]
mod frame_tests;
mod help;
mod layout;
mod measure;
mod notes;
mod panels;
mod render;
mod scene;
mod scroll;
#[cfg(test)]
mod snapshots;
mod story;
mod terms;
#[cfg(test)]
mod testing;

pub use colours::{Paint, colours_for, console_palette};
pub use draw::{bounded_rows, put, put_cells, rows};
pub use fit::{
    CUT_MARKER, Clause, Form, console_text, cut_styled, cut_words, display_width, fit_clauses,
    inert, is_console_glyph, line_width, plain, rule, tint, truncate_styled, wrap_styled,
};
pub use footer::{footer, key_hints};
pub use help::{help_body, title_rule};
pub use layout::{Layout, Rows, SizeClass, check_frame_bounds, frame_area};
pub use notes::{CoreNote, offset_list, top_note};
pub use render::render;
pub use scroll::{scroll_body_with_bar, scrollbar};
