//! The seam between the frames and the terminal loop.
//!
//! The loop owns a [`Screen`] and asks the frames to draw into a buffer whose
//! area is `(width - 1) x (height - 1)`, so the last row and the last column
//! are never written. The frames answer with [`Drawn`]: how far the view can
//! scroll and how long the drawing stays true.

use jiff::Timestamp;

/// Which page the dashboard shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// The dashboard itself.
    #[default]
    Main,
    /// The key help.
    Help,
    /// The full event log.
    Log,
}

/// How much colour the output may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colours {
    /// Every colour and attribute the terminal offers.
    Full,
    /// The Linux console (`TERM=linux`): 16 colours and no bold.
    Console,
    /// No colour at all: `NO_COLOR`, journald or a one-frame print.
    None,
}

/// Everything the frames need to know about the terminal and the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    /// The page to draw.
    pub view: View,
    /// Whether the reader can press keys, so the frames show key hints.
    pub keys: bool,
    /// Lines scrolled down from the top of the view.
    pub scroll: usize,
    /// Terminal columns.
    pub width: u16,
    /// Terminal rows.
    pub height: u16,
    /// Whether this is a single print rather than a live dashboard.
    pub one_frame: bool,
    /// The colour profile.
    pub colours: Colours,
}

/// How long a drawn frame stays true without a new journal event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// The frame shows the clock, so it is redrawn every second.
    Never,
    /// The frame stays true until this instant.
    Until(Timestamp),
    /// Nothing on the frame changes with time, so no timer runs at all.
    Frozen,
}

/// What the frames report after drawing one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drawn {
    /// The scrollable extent of the view, in lines.
    pub scroll: usize,
    /// How long the frame stays true.
    pub hold: Hold,
}
