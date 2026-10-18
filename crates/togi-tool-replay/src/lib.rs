//! The replay command: plays a recorded journal through the dashboard on a fast-forward clock, to try the dashboard
//! on a whole session in minutes, or prints one frame of it. A development program that never ships.

mod frame;
mod live;
mod load;
mod playback;

use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use jiff::SignedDuration;
use jiff::tz::TimeZone;
use togi_journal::{FileError, Refusal};
use togi_watch::model::FoldError;
use togi_watch::screen::View;
use togi_watch::terminal::ONE_FRAME_SIZE;

pub use frame::{Frame, draw};
pub use playback::{Playback, Speed, SpeedError, offset, prefix};

/// Why the replay failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("read journal")]
    Read(#[source] FileError),
    #[error("{} holds no events", .0.display())]
    Empty(PathBuf),
    #[error("replay needs a journal of this build's ruleset")]
    Refused(#[source] Refusal),
    #[error("event {seq} cannot be shown")]
    Fold {
        seq: u64,
        #[source]
        source: FoldError,
    },
    #[error("playing needs a terminal; use --at for one frame")]
    NotTerminal,
    #[error(transparent)]
    Watch(#[from] togi_watch::terminal::Error),
    #[error("write the frame")]
    Output(#[source] std::io::Error),
}

/// The page one frame shows.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Page {
    Main,
    Help,
    Log,
}

impl From<Page> for View {
    fn from(page: Page) -> Self {
        match page {
            Page::Main => Self::Main,
            Page::Help => Self::Help,
            Page::Log => Self::Log,
        }
    }
}

fn parse_after(text: &str) -> Result<SignedDuration, jiff::Error> {
    text.parse()
}

/// Play a recorded journal through the dashboard on a simulated clock, or print one frame of it.
#[derive(Debug, Parser)]
#[command(name = "togi-tool-replay", allow_negative_numbers = true)]
pub struct Cli {
    /// The state directory whose events.jsonl to play.
    #[arg(long)]
    pub state_dir: PathBuf,
    /// Simulated seconds per real second: finite and positive.
    #[arg(long, default_value = "300")]
    pub speed: Speed,
    /// Start playing at this event sequence number.
    #[arg(long, default_value_t = 1, conflicts_with = "at")]
    pub from: u64,
    /// Print one frame as of this event sequence number and exit.
    #[arg(long)]
    pub at: Option<u64>,
    /// With --at, how long after that event the frame is drawn, as in 40s or 1m30s.
    #[arg(long, requires = "at", value_parser = parse_after, allow_hyphen_values = true)]
    pub after: Option<SignedDuration>,
    /// With --at, the frame width.
    #[arg(long, requires = "at", default_value_t = ONE_FRAME_SIZE.0, value_parser = clap::value_parser!(u16).range(1..))]
    pub width: u16,
    /// With --at, the frame height.
    #[arg(long, requires = "at", default_value_t = ONE_FRAME_SIZE.1, value_parser = clap::value_parser!(u16).range(1..))]
    pub height: u16,
    /// With --at, keep the ANSI colour in the frame.
    #[arg(long, requires = "at")]
    pub color: bool,
    /// With --at, the page to draw.
    #[arg(long, requires = "at", value_enum, default_value = "main")]
    pub view: Page,
}

/// Runs the command: one frame of the recording to `out` with `--at`, else the dashboard on the terminal. What the
/// lenient read of the recording skipped is said once: on stderr after a frame, in the dashboard's footer when
/// playing, where stderr would only garble the screen.
pub fn run(cli: &Cli, out: &mut impl Write) -> Result<(), Error> {
    run_with(
        cli,
        out,
        &mut std::io::stderr(),
        load::events,
        TimeZone::system,
    )
}

/// Keeps recording access, diagnostics and local-time lookup injectable without changing playback.
fn run_with(
    cli: &Cli,
    out: &mut impl Write,
    err: &mut impl Write,
    load: impl FnOnce(&std::path::Path) -> Result<load::Loaded, Error>,
    timezone: impl FnOnce() -> TimeZone,
) -> Result<(), Error> {
    let load::Loaded { events, skipped } = load(&cli.state_dir)?;
    match cli.at {
        Some(at) => {
            let frame = Frame {
                view: cli.view.into(),
                width: cli.width,
                height: cli.height,
                colour: cli.color,
                after: cli.after.unwrap_or_default(),
            };
            let text = draw(&events, prefix(&events, at), &frame, &timezone())?;
            out.write_all(text.as_bytes())
                .and_then(|()| out.flush())
                .map_err(Error::Output)?;
            if let Some(skipped) = skipped {
                let _ = writeln!(err, "togi-tool-replay: {skipped}");
            }
            Ok(())
        }
        None => live::play(&events, prefix(&events, cli.from), cli.speed, skipped),
    }
}

#[cfg(test)]
mod tests;
