//! The dashboard frames, on ratatui buffers.
//!
//! - [`render`]: one frame of the journal on a screen, and how long it stays true.
//! - [`Paint`] and [`colours_for`]: the drawing roles the panels paint with and
//!   how each [`Colours`](crate::screen::Colours) profile applies them.
//! - [`cut_words`], [`fit_clauses`] and the rest of the fitting:
//!   every line is measured as the screen shows it, escaped, and never cut
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
#[cfg(test)]
mod hand;
mod help;
mod layout;
mod measure;
mod panels;
mod render;
mod scene;
mod scroll;
#[cfg(test)]
mod snapshots;
mod story;
#[cfg(test)]
pub(crate) mod testing;

pub use colours::{Paint, colours_for, console_palette};
pub use draw::{bounded_rows, put, put_cells, rows};
pub use fit::{
    Clause, Form, cut_marker, cut_styled, cut_words, display_width, fit_clauses, glyph_text, inert,
    is_console_glyph, line_width, plain, rule, tint, truncate_styled, wrap_styled,
};
pub use footer::{footer, key_hints};
pub use help::{help_body, title_rule};
pub use layout::{Layout, Rows, SizeClass, check_frame_bounds, frame_area};
pub use render::render;
pub use scroll::{scroll_body_with_bar, scrollbar};
