//! Journal event text: [`Renderer`] formats events and styled text using [`OutputFacts`], while
//! [`escape_text`] and [`escape_bytes`] make untrusted text safe for terminal output.

mod escape;
mod output;
#[cfg(any(test, feature = "test-support"))]
mod pty;
mod render;
mod style;

#[cfg(test)]
mod tests;

pub use escape::{Escaped, escape_bytes, escape_text};
pub use output::OutputFacts;
#[cfg(any(test, feature = "test-support"))]
pub use pty::open_pty;
pub use render::{EventLine, Renderer, StyledText};
pub use style::{Style, style_for};
pub use togi_machine::error_chain;
