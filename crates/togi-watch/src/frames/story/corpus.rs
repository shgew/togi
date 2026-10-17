//! What the narration hands the narration corpus (#904): the story and the resting band as built.

use super::{RestingBand, Story};
use crate::corpus::{shape, text};

/// The story as built: its label, its lines and its one brief line, before the narrator wraps or cuts them.
pub(super) fn said(said: &Story) {
    let form = shape(&said.label);
    text("story.label", "", &said.label);
    for each in &said.lines {
        text("story.line", &form, each);
    }
    text("story.brief", &form, &said.brief);
}

/// The resting band as built: its title and its rows, before the band is cut to its width.
pub(super) fn band(band: &RestingBand) {
    text("band.title", "", band.title);
    for each in &band.lines {
        text("band.line", band.title, each);
    }
}
