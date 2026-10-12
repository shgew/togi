//! The words of the dashboard: the tag column, the tones, and the one table that gives each kind of journal event
//! its words.

/// The tag of a line of what happened: one short word per kind of event, in a column of its own, so the eye finds
/// a crash without reading the sentence beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tag {
    /// The session started.
    Start,
    /// A trial passed.
    Pass,
    /// A trial or a core failed.
    Fail,
    /// The machine crashed.
    Crash,
    /// A trial ended without a verdict.
    Unclear,
    /// Something was skipped.
    Skip,
    /// A power loss, a reset or a reset command.
    Reset,
    /// A search step or a checking step.
    Step,
    /// A solo limit was found.
    Limit,
    /// A core has room to go deeper.
    Room,
    /// A core backed off.
    Backoff,
    /// A checking cycle.
    Cycle,
    /// A hunt.
    Hunt,
    /// A combination.
    Combo,
    /// A deepening round.
    Round,
    /// The session stopped.
    Stop,
    /// A machine-check error.
    Mce,
    /// A note.
    Note,
    /// A hunt group.
    Group,
    /// A member probe.
    Probe,
    /// A solo limit is being confirmed.
    Confirm,
    /// The session resumed.
    Resume,
}

impl Tag {
    /// The width of the tag column.
    pub const WIDTH: usize = 7;

    /// The tag as the screen writes it.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Crash => "crash",
            Self::Unclear => "unclear",
            Self::Skip => "skip",
            Self::Reset => "reset",
            Self::Step => "step",
            Self::Limit => "limit",
            Self::Room => "room",
            Self::Backoff => "backoff",
            Self::Cycle => "cycle",
            Self::Hunt => "hunt",
            Self::Combo => "combo",
            Self::Round => "round",
            Self::Stop => "stop",
            Self::Mce => "mce",
            Self::Note => "note",
            Self::Group => "group",
            Self::Probe => "probe",
            Self::Confirm => "confirm",
            Self::Resume => "resume",
        }
    }
}

/// How a line of what happened is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    /// Ordinary text.
    #[default]
    Plain,
    /// Good news.
    Good,
    /// Bad news.
    Bad,
    /// A warning.
    Warn,
    /// A hunt.
    Hunt,
    /// A combination.
    Combo,
}
