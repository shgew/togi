//! `togi watch`: the journal dashboard, with rootless day controls on its normal main view.

use std::io::{stdin, stdout};
use std::os::fd::AsFd as _;
use std::sync::LazyLock;

use clap::{Args as ClapArgs, ValueHint};
use togi_journal::DirError;
use togi_watch::screen::{Direction, Quit};
use togi_watch::terminal::{self, ONE_FRAME_SIZE, Outcome, SourceError};

use crate::cli::{Context, examples};
use crate::day::socket;
use crate::exit::Exit;
use crate::night::{self, Confirmation};
use crate::night_end;
use crate::read::unknown_kind;
use crate::report::fail;

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

/// Show the session dashboard with rootless stock-only day controls
///
/// It redraws when the journal changes and on whole-second boundaries for the clock. During an idle trial it
/// holds still, clock included, until the journal changes, a key is pressed or the terminal is
/// resized. togi says in plain words what it is doing and why, how far the current trial has
/// come and what each outcome would lead to. Each CCD shows its cores' offsets beside its night
/// edge U, cap S and applied day offset D; a context panel shows when the night started, its
/// elapsed observed time and each queue's share of it, qualification and reproductions, or the
/// day's hold, step, freeze, firmware, events and vetoes, beside what happened recently. On a
/// terminal, keys switch views: `?` explains the screen, `l` shows the newest 400 journal events,
/// Up/Down and PageUp/PageDown scroll both, Esc returns, and `q` exits the dashboard. A journal scrolled back holds its list still
/// and counts new events until End. It is rendered from the journal, which it reloads only when
/// it changes.
/// On the normal main view, `p` toggles a stock-only pause until cold boot and `f` toggles freeze.
/// These controls use the credential-checked day socket without root; `f` still scrolls help and log.
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
/// `--tuning-boot`, which keeps it open: `q`, `Q`, Ctrl-C and Ctrl-D do nothing in every view and the help and
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

    /// Run as the unattended tuning-boot dashboard: ignore q, Q, Ctrl-C and Ctrl-D in every view and leave the quit key out of the help and footer
    #[arg(long)]
    tuning_boot: bool,
}

/// Runs `togi watch`.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    // Another build's journal is refused before the terminal is touched and before any frame: no frame, no retry.
    if let Err(error @ DirError::ForeignJournal { .. }) = ctx.system.recorded_build(ctx.state_dir) {
        return fail(ctx, "watch", togi_render::error_chain(&error));
    }
    let system = ctx.system;
    match dashboard(|| show(args, ctx), |request| system.day_request(request)) {
        Ok(Leave::Ended) => Exit::Ok,
        Ok(Leave::Switch(Direction::Night)) => night::begin(ctx, Confirmation::Given),
        Ok(Leave::Switch(Direction::Day)) => night_end::leave(ctx),
        Err(Stop::Screen(terminal::Error::Journal(SourceError::Read(error)))) => {
            let message = unknown_kind(&error).unwrap_or_else(|| togi_render::error_chain(&error));
            fail(ctx, "watch", message)
        }
        Err(Stop::Screen(error)) => fail(ctx, "watch", togi_render::error_chain(&error)),
        Err(Stop::Control(error)) => fail(ctx, "watch", togi_render::error_chain(&error)),
    }
}

/// Shows the dashboard once: full screen on a terminal, otherwise one frame.
fn show(args: &Args, ctx: &mut Context<'_>) -> Result<Outcome, terminal::Error> {
    if ctx.system.stdout_facts().is_terminal {
        let out = stdout();
        let keys = stdin();
        terminal::run(
            ctx.state_dir,
            out.as_fd(),
            Some(keys.as_fd()),
            args.quit(),
            offer(args, ctx),
        )
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
        .map(|()| Outcome::Ended)
    } else {
        terminal::one_frame(ctx.state_dir, args.width, args.height, &mut ctx.out)
            .map(|()| Outcome::Ended)
    }
}

/// Why the dashboard left its screen for good.
#[derive(Debug, PartialEq, Eq)]
enum Leave {
    Ended,
    Switch(Direction),
}

/// What stopped the dashboard with an error.
#[derive(Debug)]
enum Stop {
    Screen(terminal::Error),
    Control(ControlError),
}

/// A day control key the day service did not carry out.
#[derive(Debug, thiserror::Error)]
enum ControlError {
    #[error(transparent)]
    Socket(#[from] socket::Error),
    #[error("unexpected day service reply")]
    Unexpected,
}

/// Shows the dashboard until it ends or switches. Each day control key is carried out through `exchange`, one
/// day-socket request and its reply, and the dashboard is then shown again; a control that fails ends the watch.
fn dashboard(
    mut show: impl FnMut() -> Result<Outcome, terminal::Error>,
    mut exchange: impl FnMut(&socket::Request) -> Result<socket::Reply, socket::Error>,
) -> Result<Leave, Stop> {
    loop {
        match show().map_err(Stop::Screen)? {
            Outcome::Ended => return Ok(Leave::Ended),
            Outcome::Switch(direction) => return Ok(Leave::Switch(direction)),
            Outcome::Control(action) => control(action, &mut exchange).map_err(Stop::Control)?,
        }
    }
}

fn control_request(
    action: terminal::DayControl,
    status: &crate::day::socket::ControlStatus,
) -> crate::day::socket::Request {
    use crate::day::socket::Request;
    match action {
        terminal::DayControl::Pause => match status.pause {
            togi_journal::PauseState::Running {} => Request::Pause(socket::PauseRequest {
                state: togi_journal::PauseState::UntilBoot {},
            }),
            togi_journal::PauseState::UntilBoot {} | togi_journal::PauseState::Until { .. } => {
                Request::Resume
            }
        },
        terminal::DayControl::Freeze => Request::Freeze(socket::FreezeRequest {
            frozen: !status.frozen,
        }),
    }
}

/// Asks the day service for its state, then sends the control the key means in that state.
fn control(
    action: terminal::DayControl,
    exchange: &mut impl FnMut(&socket::Request) -> Result<socket::Reply, socket::Error>,
) -> Result<(), ControlError> {
    use socket::{Reply, Request};
    let status = match exchange(&Request::State)? {
        Reply::State { status } => status,
        Reply::Refused { reason } => return Err(socket::Error::Refused(reason).into()),
        Reply::Questions { .. }
        | Reply::Controlled { .. }
        | Reply::Answered
        | Reply::Recorded { .. }
        | Reply::Reapplied => return Err(ControlError::Unexpected),
    };
    match exchange(&control_request(action, &status))? {
        Reply::Controlled { .. } => Ok(()),
        Reply::Refused { reason } => Err(socket::Error::Refused(reason).into()),
        Reply::Questions { .. }
        | Reply::State { .. }
        | Reply::Answered
        | Reply::Recorded { .. }
        | Reply::Reapplied => Err(ControlError::Unexpected),
    }
}

/// The night switch the dashboard offers: into the night from a normal boot, back to the day from the tuning
/// boot, and none when the host has no tuning boot set up or the reader cannot change the boot loader.
pub fn offer(args: &Args, ctx: &Context<'_>) -> Option<Direction> {
    if !ctx.system.privileged() || night::setup(ctx).is_err() {
        return None;
    }
    Some(if args.tuning_boot {
        Direction::Day
    } else {
        Direction::Night
    })
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
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;

    use clap::Parser;
    use togi_journal::PauseState;
    use togi_watch::screen::{Direction, Quit};
    use togi_watch::terminal::{DayControl, Outcome};

    use super::{Args, Leave, Stop, dashboard};
    use crate::day::socket::{ControlStatus, Error, Reply};

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

    #[test_case::test_case(&["--tuning-boot=true"]; "equals value")]
    #[test_case::test_case(&["--tuning-boot", "1"]; "positional value")]
    #[test_case::test_case(&["--tuning_boot"]; "underscore")]
    fn the_tuning_boot_flag_takes_no_value(args: &[&str]) {
        assert!(parse(args).is_err(), "{args:?}");
    }

    fn status(pause: PauseState, frozen: bool) -> ControlStatus {
        ControlStatus {
            pause,
            frozen,
            revision: None,
        }
    }

    /// The dashboard's two seams, scripted: the screens it is shown and the day-socket replies it gets.
    struct Script {
        screens: RefCell<VecDeque<Outcome>>,
        replies: RefCell<VecDeque<Result<Reply, Error>>>,
        shown: RefCell<usize>,
        sent: RefCell<Vec<serde_json::Value>>,
    }

    impl Script {
        fn new(
            screens: impl IntoIterator<Item = Outcome>,
            replies: impl IntoIterator<Item = Result<Reply, Error>>,
        ) -> Self {
            Self {
                screens: RefCell::new(screens.into_iter().collect()),
                replies: RefCell::new(replies.into_iter().collect()),
                shown: RefCell::new(0),
                sent: RefCell::new(Vec::new()),
            }
        }

        fn run(&self) -> Result<Leave, Stop> {
            dashboard(
                || {
                    *self.shown.borrow_mut() += 1;
                    Ok(self
                        .screens
                        .borrow_mut()
                        .pop_front()
                        .expect("the dashboard was shown more often than scripted"))
                },
                |request| {
                    self.sent
                        .borrow_mut()
                        .push(serde_json::to_value(request).unwrap());
                    self.replies
                        .borrow_mut()
                        .pop_front()
                        .expect("the day socket was asked more often than scripted")
                },
            )
        }
    }

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).unwrap()
    }

    #[test_case::test_case(DayControl::Pause, PauseState::Running {}, false, r#"{"pause":{"state":{"kind":"until_boot"}}}"#; "start untimed pause")]
    #[test_case::test_case(DayControl::Pause, PauseState::UntilBoot {}, false, r#""resume""#; "end untimed pause")]
    #[test_case::test_case(DayControl::Pause, PauseState::Until { deadline: jiff::Timestamp::UNIX_EPOCH }, false, r#""resume""#; "end timed pause")]
    #[test_case::test_case(DayControl::Freeze, PauseState::UntilBoot {}, false, r#"{"freeze":{"frozen":true}}"#; "freeze keeps pause")]
    #[test_case::test_case(DayControl::Freeze, PauseState::UntilBoot {}, true, r#"{"freeze":{"frozen":false}}"#; "unfreeze keeps pause")]
    fn a_day_key_sends_the_request_its_status_calls_for_and_shows_the_dashboard_again(
        action: DayControl,
        pause: PauseState,
        frozen: bool,
        expected: &str,
    ) {
        let now = status(pause, frozen);
        let script = Script::new(
            [Outcome::Control(action), Outcome::Ended],
            [
                Ok(Reply::State {
                    status: now.clone(),
                }),
                Ok(Reply::Controlled { status: now }),
            ],
        );
        assert!(matches!(script.run(), Ok(Leave::Ended)));
        assert_eq!(*script.sent.borrow(), [json(r#""state""#), json(expected)]);
        assert_eq!(*script.shown.borrow(), 2);
    }

    #[test]
    fn a_night_switch_ends_the_dashboard_without_asking_the_day_socket() {
        let script = Script::new([Outcome::Switch(Direction::Night)], []);
        assert!(matches!(script.run(), Ok(Leave::Switch(Direction::Night))));
        assert_eq!(*script.sent.borrow(), Vec::<serde_json::Value>::new());
    }

    fn refused() -> Error {
        Error::Refused("pause needs the day service".into())
    }

    #[test_case::test_case(Err(refused()), "day service refused the request: pause needs the day service"; "socket refusal")]
    #[test_case::test_case(Ok(Reply::Refused { reason: "frozen by hand".into() }), "day service refused the request: frozen by hand"; "refused reply")]
    #[test_case::test_case(Err(Error::Absent(io::Error::other("no socket"))), "reach the day service: no socket"; "no day service")]
    #[test_case::test_case(Ok(Reply::Answered), "unexpected day service reply"; "wrong reply")]
    fn a_failed_control_ends_the_watch_with_the_reason_and_does_not_reopen_it(
        reply: Result<Reply, Error>,
        message: &str,
    ) {
        let script = Script::new(
            [Outcome::Control(DayControl::Pause)],
            [
                Ok(Reply::State {
                    status: status(PauseState::Running {}, false),
                }),
                reply,
            ],
        );
        let Err(Stop::Control(error)) = script.run() else {
            panic!("a control that failed must end the watch");
        };
        assert_eq!(togi_render::error_chain(&error), message);
        assert_eq!(script.sent.borrow().len(), 2);
        assert_eq!(*script.shown.borrow(), 1);
    }

    #[test_case::test_case(Err(Error::Absent(io::Error::other("no socket"))), "reach the day service: no socket"; "no day service")]
    #[test_case::test_case(Err(refused()), "day service refused the request: pause needs the day service"; "socket refusal")]
    #[test_case::test_case(Ok(Reply::Refused { reason: "not yours".into() }), "day service refused the request: not yours"; "refused reply")]
    #[test_case::test_case(Ok(Reply::Reapplied), "unexpected day service reply"; "wrong reply")]
    fn a_failed_status_query_sends_no_control(reply: Result<Reply, Error>, message: &str) {
        let script = Script::new([Outcome::Control(DayControl::Freeze)], [reply]);
        let Err(Stop::Control(error)) = script.run() else {
            panic!("a failed status query must end the watch");
        };
        assert_eq!(togi_render::error_chain(&error), message);
        assert_eq!(*script.sent.borrow(), [json(r#""state""#)]);
        assert_eq!(*script.shown.borrow(), 1);
    }
}
