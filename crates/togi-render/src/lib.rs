mod escape;
mod output;
mod render;
mod style;

#[cfg(test)]
mod tests;

pub use escape::{Escaped, escape_bytes, escape_text};
pub use output::OutputFacts;
pub use render::{EventLine, Renderer, StyledText};
pub use style::{Style, style_for};
