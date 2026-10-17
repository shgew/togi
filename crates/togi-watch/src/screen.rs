//! The seam between the frames and the terminal loop.
//!
//! The loop owns a [`Screen`] and asks the frames to draw into a buffer whose
//! area is `(width - 1) x (height - 1)`, so the last row and the last column
//! are never written. The frames answer with [`Drawn`]: how far the view can
//! scroll and how long the drawing stays true.

use jiff::Timestamp;

use crate::history::LogEntry;
use crate::model::Snapshot;

/// Which page the dashboard shows. The pages form one fixed cycle in declaration order, which Left and Right
/// walk with wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// Phase progress, the operation now, every core's gauge and a short What happened.
    #[default]
    Main,
    /// Each core's exact values, roles and failure evidence.
    Cores,
    /// The current phase's work list: search turns, the cycle checklist or the phase 2 round.
    Cycle,
    /// R7 request order, top requesters and self-sufficiency.
    R7,
    /// Hunts and probes: cause, attribution, groups, splits and probe evidence.
    Hunts,
    /// The profile to enter in BIOS.
    Bios,
    /// What happened, expanded.
    History,
    /// The journal's events.
    Log,
    /// The key and term help.
    Help,
}

impl View {
    /// Every page in cycle order.
    pub const ALL: [Self; 9] = [
        Self::Main,
        Self::Cores,
        Self::Cycle,
        Self::R7,
        Self::Hunts,
        Self::Bios,
        Self::History,
        Self::Log,
        Self::Help,
    ];

    /// The page's position in the cycle, from 1.
    #[must_use]
    pub const fn number(self) -> usize {
        match self {
            Self::Main => 1,
            Self::Cores => 2,
            Self::Cycle => 3,
            Self::R7 => 4,
            Self::Hunts => 5,
            Self::Bios => 6,
            Self::History => 7,
            Self::Log => 8,
            Self::Help => 9,
        }
    }

    /// The page after this one, wrapping from the last to the first.
    #[must_use]
    pub const fn next(self) -> Self {
        Self::ALL[self.number() % Self::ALL.len()]
    }

    /// The page before this one, wrapping from the first to the last.
    #[must_use]
    pub const fn prev(self) -> Self {
        Self::ALL[(self.number() + Self::ALL.len() - 2) % Self::ALL.len()]
    }

    /// The page's name as titles and the footer show it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Main => "Main",
            Self::Cores => "Cores",
            Self::Cycle => "Cycle",
            Self::R7 => "R7 evidence",
            Self::Hunts => "Hunts / probes",
            Self::Bios => "BIOS profile",
            Self::History => "History",
            Self::Log => "Journal",
            Self::Help => "Help",
        }
    }
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
