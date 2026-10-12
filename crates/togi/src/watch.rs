//! `togi watch`: the read-only dashboard of the journal.

use std::io::{IsTerminal as _, stdin, stdout};
use std::os::fd::AsFd as _;
use std::sync::LazyLock;

use clap::Args as ClapArgs;
use togi_render::escape_text;
use togi_watch::terminal::{self, ONE_FRAME_SIZE};

use crate::cli::{Context, examples};
use crate::exit::Exit;

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[
        (
            "togi watch",
            "The session in the default state directory, full screen",
        ),
        (
            "togi --state-dir <dir> watch > frame.txt",
            "One 240x67 frame of the session in <dir>",
        ),
    ])
});

/// Show the session as a read-only dashboard
///
/// It redraws when the journal changes and once a second for the clock. During an idle trial it
/// holds still, clock included, until the journal changes, a key is pressed or the terminal is
/// resized. togi says in plain words what it is doing and why, how far the current trial and cycle
/// have come, and what each outcome would lead to. Each CCD's cores show their offsets and limits
/// side by side; below them, the cycle, hunt or search beside what happened recently. On a
/// terminal, keys switch views: `?` explains the screen, `l` shows the newest 400 journal events,
/// the arrow and page keys scroll both, `q` quits. A journal scrolled back holds its list still
/// and counts new events until End. It is rendered from the journal, which it reloads only when
/// it changes.
///
/// On a terminal it fills the screen until interrupted; otherwise it prints one frame of --width
/// by --height and exits. An unreadable or incompatible journal appears in the frame and on
/// stderr, and one-frame watch exits 1; live watch keeps showing the problem and retries when the
/// journal changes. A missing journal (no session yet) is not an error. The tuning boot shows it
/// on tty1. `NO_COLOR` turns colour off.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    /// Frame width in columns when stdout is not a terminal
    #[arg(
        long,
        value_name = "columns",
        default_value_t = ONE_FRAME_SIZE.0,
        value_parser = clap::value_parser!(u16).range(1..),
    )]
    width: u16,

    /// Frame height in rows when stdout is not a terminal
    #[arg(
        long,
        value_name = "rows",
        default_value_t = ONE_FRAME_SIZE.1,
        value_parser = clap::value_parser!(u16).range(1..),
    )]
    height: u16,
}

/// Runs `togi watch`.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    let result = if stdout().is_terminal() {
        let out = stdout();
        let keys = stdin();
        terminal::run(ctx.state_dir, out.as_fd(), Some(keys.as_fd()))
    } else {
        terminal::one_frame(ctx.state_dir, args.width, args.height, &mut ctx.out)
    };
    match result {
        Ok(()) => Exit::Ok,
        Err(error) => ctx.error(format_args!("watch: {}", escape_text(&error.to_string()))),
    }
}
