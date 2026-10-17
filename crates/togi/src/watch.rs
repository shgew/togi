//! `togi watch`: the read-only dashboard of the journal.

use std::io::{stdin, stdout};
use std::os::fd::AsFd as _;
use std::sync::LazyLock;

use clap::{Args as ClapArgs, ValueHint};
use togi_journal::DirError;
use togi_watch::screen::Quit;
use togi_watch::terminal::{self, ONE_FRAME_SIZE, SourceError};

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::read::unknown_kind;
use crate::report::{chain, fail};

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
/// the arrow and page keys scroll both, `q` exits the dashboard. A journal scrolled back holds its list still
/// and counts new events until End. It is rendered from the journal, which it reloads only when
/// it changes.
///
/// The screen's words are explained by `?` on the screen itself; the rules behind them are in
/// docs/spec/tuner.md.
///
/// On a terminal it fills the screen until interrupted; otherwise it prints one frame of --width
/// by --height and exits. An unreadable or incompatible journal appears in the frame and on
/// stderr, and one-frame watch exits 1; live watch keeps showing the problem and retries when the
/// journal changes. A journal another build wrote, whose first line is not a togi journal header,
/// is refused before any frame: exit 1 with the cutover step on stderr, live or not. A missing
/// journal (no session yet) is not an error. An event of a kind this build does not know is
/// skipped and an unknown field is ignored; the footer says which. The tuning boot shows it on tty1 with
/// `--tuning-boot`, which keeps it open: `q`, `Q`, Ctrl-C and Ctrl-D do nothing on every page and the help and
/// footer leave the quit key out, so only stopping the service, or a signal such as SIGTERM, ends it. A terminal
/// alone never sets it. `NO_COLOR` turns colour off.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    /// Frame width in columns, 1 to 1000, when stdout is not a terminal
    #[arg(
        long,
        value_name = "columns",
        value_hint = ValueHint::Other,
        default_value_t = ONE_FRAME_SIZE.0,
        value_parser = clap::value_parser!(u16).range(1..=1000),
    )]
    width: u16,

    /// Frame height in rows, 1 to 1000, when stdout is not a terminal
    #[arg(
        long,
        value_name = "rows",
        value_hint = ValueHint::Other,
        default_value_t = ONE_FRAME_SIZE.1,
        value_parser = clap::value_parser!(u16).range(1..=1000),
    )]
    height: u16,

    /// Run as the unattended tuning-boot dashboard: ignore q, Q, Ctrl-C and Ctrl-D on every page and leave the quit key out of the help and footer
    #[arg(long)]
    tuning_boot: bool,
}

/// Runs `togi watch`.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    // Another build's journal is refused before the terminal is touched and before any frame: no frame, no retry.
    if let Err(error @ DirError::ForeignJournal { .. }) = ctx.system.recorded_build(ctx.state_dir) {
        return fail(ctx, "watch", chain(&error));
    }
    let result = if ctx.system.stdout_facts().is_terminal {
        let out = stdout();
        let keys = stdin();
        terminal::run(ctx.state_dir, out.as_fd(), Some(keys.as_fd()), args.quit())
    } else if let Some(fs) = ctx.system.journal_fs() {
        terminal::one_frame_in(
            fs.as_ref(),
            ctx.state_dir,
            args.width,
            args.height,
            &mut ctx.out,
            ctx.system.now(),
            &ctx.system.zone(),
        )
    } else {
        terminal::one_frame(ctx.state_dir, args.width, args.height, &mut ctx.out)
    };
    match result {
        Ok(()) => Exit::Ok,
        Err(terminal::Error::Journal(SourceError::Read(error))) => {
            let message = unknown_kind(&error).unwrap_or_else(|| error.to_string());
            fail(ctx, "watch", message)
        }
        Err(error) => fail(ctx, "watch", error),
    }
}

impl Args {
    /// What the quit keys do: ignored only where the caller said so with `--tuning-boot`, never inferred from the
    /// terminal.
    fn quit(&self) -> Quit {
        if self.tuning_boot {
            Quit::Ignored
        } else {
            Quit::Keys
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use togi_watch::screen::Quit;

    use super::Args;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        watch: Args,
    }

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Cli::try_parse_from(std::iter::once("watch").chain(args.iter().copied()))
            .map(|cli| cli.watch)
    }

    #[test]
    fn watch_keeps_the_quit_keys_unless_the_tuning_boot_is_named() {
        assert_eq!(parse(&[]).unwrap().quit(), Quit::Keys);
        assert_eq!(parse(&["--width", "80"]).unwrap().quit(), Quit::Keys);
        assert_eq!(parse(&["--tuning-boot"]).unwrap().quit(), Quit::Ignored);
        let both = parse(&["--tuning-boot", "--width", "80", "--height", "20"]).unwrap();
        assert_eq!(
            (both.quit(), both.width, both.height),
            (Quit::Ignored, 80, 20)
        );
    }

    #[test]
    fn the_tuning_boot_flag_takes_no_value() {
        for args in [
            &["--tuning-boot=true"][..],
            &["--tuning-boot", "1"],
            &["--tuning_boot"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }
}
