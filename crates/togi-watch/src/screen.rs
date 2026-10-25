//! The seam between the frames and the terminal loop.
//!
//! The loop owns a [`Screen`] and asks the frames to draw into a buffer whose
//! area is `(width - 1) x (height - 1)`, so the last row and the last column
//! are never written. The frames answer with [`Drawn`]: how far the view can
//! scroll and how long the drawing stays true.

use jiff::Timestamp;

use crate::history::LogEntry;
use crate::model::Snapshot;

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
    /// The terminal's named colour slots and console-compatible glyphs.
    Full,
    /// Existing console RGB colours emitted directly, with Unicode glyphs.
    Rich,
    /// The Linux console (`TERM=linux`): 16 colours and no bold.
    Console,
    /// No colour at all: `NO_COLOR`, journald or a one-frame print.
    None,
}

/// What the quit keys `q`, `Q`, Ctrl-C and Ctrl-D do. The caller decides it explicitly; it is never inferred from the
/// terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quit {
    /// They exit the dashboard and its hints name `q`: `togi watch` started by hand.
    Keys,
    /// They do nothing on every page and no hint names them: the tuning boot's dashboard service, which only its
    /// service stop or a signal ends.
    Ignored,
}

/// Which way the machine's next reboot goes across the night switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From the day into the tuning boot: offered in a normal boot.
    Night,
    /// From the tuning boot back into the day: offered in the night.
    Day,
}

/// The reboot the dashboard offers and whether it is asking about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// No reboot is offered: the dashboard has no night setup to switch with.
    Off,
    /// The key for `Direction` is shown and asks the question when pressed.
    Offered(Direction),
    /// The question for `Direction` is on screen: `y` answers it, any other key withdraws it.
    Asking(Direction),
}

/// Everything the frames need to know about the terminal and the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    /// The page to draw.
    pub view: View,
    /// Whether the reader can press keys, so the frames show key hints.
    pub keys: bool,
    /// Lines scrolled down from the top of the view. `usize::MAX` means following the end; the frames clamp any
    /// scroll at or past the extent to the end.
    pub scroll: usize,
    /// Terminal columns.
    pub width: u16,
    /// Terminal rows.
    pub height: u16,
    /// Whether this is a single print rather than a live dashboard.
    pub one_frame: bool,
    /// The colour profile.
    pub colours: Colours,
    /// What the quit keys do, and so whether the hints name them.
    pub quit: Quit,
    /// The night switch the footer offers and the question it may be asking.
    pub switch: Switch,
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

/// The journal list the log view shows: the model's own, or one the loop held when the reader scrolled back.
#[derive(Debug, Clone, Copy)]
pub struct Log<'a> {
    /// The lines, oldest first.
    pub entries: &'a [LogEntry],
    /// Every event the log lists, kept or not.
    pub total: usize,
    /// How many events arrived after the list was held.
    pub arrived: usize,
}

/// What the loop knows about the journal, which the frames draw from.
#[expect(
    clippy::large_enum_variant,
    reason = "one is built per frame and borrowed; the snapshot is a bag of references"
)]
#[derive(Debug, Clone, Copy)]
pub enum Journal<'a> {
    /// There is no journal.
    Missing,
    /// A journal a session is about to append to, before it has recorded anything: nothing in it describes this
    /// run.
    Starting,
    /// The journal cannot be read or folded.
    Problem(&'a (dyn std::error::Error + 'a)),
    /// The journal, folded.
    Ready {
        /// The facts.
        snapshot: Snapshot<'a>,
        /// The log list.
        log: Log<'a>,
    },
}
