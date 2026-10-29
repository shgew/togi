//! Desktop notification adapter; the durable question remains in the journal after dismissal.

use std::collections::BTreeSet;
use std::time::Duration;

use togi_command::Invocation;
use togi_journal::{DayAnswerKind, DayUncleanEnd};
use togi_machine::{CancellationToken, Clock};

use super::socket::{Reply, Request};
use crate::cli::Context;
use crate::exit::Exit;
use crate::report::exit_with;

fn invocation(end: &DayUncleanEnd, actions: bool) -> Invocation {
    let mut command = Invocation::new("notify-send")
        .args([
            "--app-name=togi",
            "--icon=dialog-question",
            "--expire-time=120000",
        ])
        .output_limit(4096);
    if actions {
        command = command.arg("--wait");
        for kind in togi_watch::day::ANSWERS {
            command = command.arg(format!(
                "--action={}={}",
                togi_watch::day::answer_arg(kind),
                togi_watch::day::choice_words(kind)
            ));
        }
    }
    command.arg("What happened before this boot ended?").arg(format!(
        "{}\nAnswer in this notification or with togi day answer {} frozen|graphics|power-off|unsure. The question stays in togi status and togi watch until answered.",
        end.summary(), end.boot
    ))
}

fn morning_invocation(report: &crate::morning::Report) -> Invocation {
    Invocation::new("notify-send")
        .args([
            "--app-name=togi",
            "--icon=dialog-information",
            "--expire-time=120000",
        ])
        .output_limit(4096)
        .arg("Last night's evidence")
        .arg(&report.text)
}

fn selected(bytes: &[u8]) -> Option<DayAnswerKind> {
    let value = std::str::from_utf8(bytes).ok()?.strip_suffix('\n')?;
    super::parse_answer(value).ok()
}

pub(super) fn run(ctx: &mut Context<'_>) -> Exit {
    let cancel = CancellationToken::new();
    let _signals = match ctx.system.listen(&cancel) {
        Ok(signals) => signals,
        Err(error) => {
            return exit_with(
                ctx,
                "day notify",
                togi_render::error_chain(&error),
                Exit::Error,
            );
        }
    };
    let mut clock = crate::system::SystemClock(ctx.system);
    serve(ctx, &cancel, &mut clock)
}

fn serve(ctx: &mut Context<'_>, cancel: &CancellationToken, clock: &mut impl Clock) -> Exit {
    let mut sent = BTreeSet::new();
    let mut morning_sent = BTreeSet::new();
    loop {
        let (pending, morning) = match ctx.system.day_request(&Request::Questions) {
            Ok(Reply::Questions { pending, morning }) => (pending, morning),
            Ok(Reply::Answered | Reply::Refused { .. }) => {
                return exit_with(
                    ctx,
                    "day notify",
                    "unexpected day service reply",
                    Exit::Error,
                );
            }
            Err(error) => {
                return exit_with(
                    ctx,
                    "day notify",
                    togi_render::error_chain(&error),
                    Exit::Error,
                );
            }
        };
        if let Some(report) = morning.filter(|report| !morning_sent.contains(&report.boot)) {
            match ctx.system.day_notification(&morning_invocation(&report)) {
                Ok(output) if output.status.success() => {
                    morning_sent.insert(report.boot);
                }
                Ok(_) => return exit_with(ctx, "day notify", "notify-send failed", Exit::Error),
                Err(error) => {
                    return exit_with(
                        ctx,
                        "day notify",
                        togi_render::error_chain(&error),
                        Exit::Error,
                    );
                }
            }
        }
        for end in &pending {
            if sent.contains(&end.boot) {
                continue;
            }
            // Plasma supports freedesktop actions. Other desktops receive a plain notification;
            // their users still have the durable status/watch question and unprivileged CLI.
            let output = match ctx
                .system
                .day_notification(&invocation(end, ctx.system.day_notification_actions()))
            {
                Ok(output) if output.status.success() => output,
                Ok(_) => return exit_with(ctx, "day notify", "notify-send failed", Exit::Error),
                Err(error) => {
                    return exit_with(
                        ctx,
                        "day notify",
                        togi_render::error_chain(&error),
                        Exit::Error,
                    );
                }
            };
            sent.insert(end.boot.clone());
            if let Some(answer) = selected(&output.stdout) {
                match ctx.system.day_request(&Request::Answer {
                    boot: end.boot.clone(),
                    answer,
                }) {
                    Ok(Reply::Answered) => {}
                    Ok(Reply::Questions { .. } | Reply::Refused { .. }) => {
                        return exit_with(
                            ctx,
                            "day notify",
                            "unexpected day service reply",
                            Exit::Error,
                        );
                    }
                    Err(error) => {
                        return exit_with(
                            ctx,
                            "day notify",
                            togi_render::error_chain(&error),
                            Exit::Error,
                        );
                    }
                }
            }
        }
        match clock.sleep(cancel, Duration::from_secs(10)) {
            Ok(()) => {}
            Err(togi_machine::Error::Cancelled) => return Exit::Ok,
            Err(error) => {
                return exit_with(
                    ctx,
                    "day notify",
                    togi_render::error_chain(&error),
                    Exit::Error,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(b"frozen\n", Some(DayAnswerKind::Frozen))]
    #[test_case(b"graphics\n", Some(DayAnswerKind::Graphics))]
    #[test_case(b"power-off\n", Some(DayAnswerKind::PowerOff))]
    #[test_case(b"unsure\n", Some(DayAnswerKind::Unsure))]
    #[test_case(b"", None; "dismissal is not an answer")]
    #[test_case(b"frozen\nunsure\n", None; "multiple actions refused")]
    fn only_a_known_single_action_answers(output: &[u8], answer: Option<DayAnswerKind>) {
        assert_eq!(selected(output), answer);
    }

    #[test_case(true, &[
        "--wait",
        "--action=frozen=it froze",
        "--action=graphics=graphics died but sound went on",
        "--action=power-off=I turned it off while it was working normally",
        "--action=unsure=not sure",
    ]; "plasma has all four actions")]
    #[test_case(false, &[]; "other desktops degrade without actions")]
    fn installed_cli_receives_the_action_contract(actions: bool, flags: &[&str]) {
        let end = DayUncleanEnd {
            boot: "crashed-boot".to_owned().into(),
            last_checkpoint: None,
            offsets: togi_machine::PerCore::default(),
            reset_reason: togi_machine::ResetReason {
                kind: None,
                raw: Vec::new(),
                supported: false,
            },
            evidence: togi_machine::EndEvidence::unknown(),
        };
        let expected = Invocation::new("notify-send")
            .args(["--app-name=togi", "--icon=dialog-question", "--expire-time=120000"])
            .output_limit(4096)
            .args(flags.iter().copied())
            .arg("What happened before this boot ended?")
            .arg(format!(
                "{}\nAnswer in this notification or with togi day answer crashed-boot frozen|graphics|power-off|unsure. The question stays in togi status and togi watch until answered.",
                end.summary(),
            ));
        assert_eq!(invocation(&end, actions), expected);
    }

    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::os::unix::process::ExitStatusExt as _;
    use std::path::Path;
    use std::process::ExitStatus;
    use std::sync::Arc;

    use jiff::Timestamp;
    use jiff::tz::TimeZone;
    use togi_command::Output;
    use togi_hostlock::{HostLock, Policy};
    use togi_machine::BootId;
    use togi_render::OutputFacts;

    use crate::dashboard::Surface;
    use crate::day::socket::Error as SocketError;
    use crate::system::System;

    #[derive(Debug, PartialEq)]
    enum Seen {
        Questions,
        Answer(BootId, DayAnswerKind),
    }

    /// A desktop session whose day service and notification daemon answer from scripts.
    #[derive(Default)]
    struct Desktop {
        actions: bool,
        listen_error: RefCell<Option<crate::signals::Error>>,
        token: RefCell<Option<CancellationToken>>,
        replies: RefCell<VecDeque<Result<Reply, SocketError>>>,
        notifications: RefCell<VecDeque<Result<Output, togi_command::Error>>>,
        requests: RefCell<Vec<Seen>>,
        shown: RefCell<Vec<Invocation>>,
    }

    impl Desktop {
        fn new(
            replies: impl IntoIterator<Item = Result<Reply, SocketError>>,
            notifications: impl IntoIterator<Item = Result<Output, togi_command::Error>>,
        ) -> Self {
            Self {
                replies: RefCell::new(replies.into_iter().collect()),
                notifications: RefCell::new(notifications.into_iter().collect()),
                ..Self::default()
            }
        }
    }

    impl System for Desktop {
        fn check_platform(&self) -> Result<(), togi_hardware::Error> {
            Ok(())
        }

        fn privileged(&self) -> bool {
            false
        }

        fn listen(
            &self,
            token: &CancellationToken,
        ) -> Result<Box<dyn std::any::Any>, crate::signals::Error> {
            if let Some(error) = self.listen_error.borrow_mut().take() {
                return Err(error);
            }
            *self.token.borrow_mut() = Some(token.clone());
            Ok(Box::new(()))
        }

        fn acquire_host_lock(&self) -> Result<HostLock, togi_hostlock::Error> {
            Err(togi_hostlock::Error::Io {
                path: Path::new("/lock").to_owned(),
                operation: togi_hostlock::Operation::Lock,
                source: std::io::Error::other("test host has no host lock"),
            })
        }

        fn host_lock(&self) -> (&Path, Policy) {
            (Path::new("/lock"), Policy::Private)
        }

        fn config_path(&self) -> &Path {
            Path::new("/config")
        }

        fn boot_id(&self) -> Result<BootId, togi_detect::Error> {
            panic!("notify does not read the boot id")
        }

        fn now(&self) -> Timestamp {
            Timestamp::UNIX_EPOCH
        }

        fn monotonic(&self) -> Result<std::time::Duration, togi_machine::Error> {
            Ok(std::time::Duration::ZERO)
        }

        fn zone(&self) -> TimeZone {
            TimeZone::UTC
        }

        fn topology(&self) -> Result<Vec<togi_machine::CoreInfo>, togi_hardware::Error> {
            panic!("notify does not read the topology")
        }

        fn diagnose(
            &self,
            _: &togi_config::Config,
            _: Option<&togi_machine::BiosContext>,
            _: Option<&HostLock>,
        ) -> Result<togi_hardware::Diagnosis, togi_hardware::Error> {
            panic!("notify does not diagnose hardware")
        }

        fn diagnose_day(
            &self,
            _: &togi_config::Config,
            _: Option<&HostLock>,
        ) -> Result<togi_hardware::Diagnosis, togi_hardware::Error> {
            panic!("notify does not diagnose day hardware")
        }

        fn bootloader(&self, _: &Path) -> std::io::Result<Box<dyn togi_tuningboot::Bootloader>> {
            panic!("notify does not touch the bootloader")
        }

        fn create_retry_marker(&self) -> std::io::Result<()> {
            panic!("notify does not create the retry marker")
        }

        fn remove_retry_marker(&self) -> std::io::Result<()> {
            panic!("notify does not remove the retry marker")
        }

        fn stop_tuning(&self) -> Result<(), togi_hardware::StopError> {
            Err(togi_hardware::StopError::Exit {
                status: ExitStatus::from_raw(1 << 8),
                stderr: "test host has no tuning service".into(),
            })
        }

        fn answer(&self) -> std::io::Result<Option<String>> {
            Err(std::io::Error::other("test host has no terminal answer"))
        }

        fn prompt_line(&self) -> std::io::Result<Option<String>> {
            Err(std::io::Error::other("test host has no terminal input"))
        }

        fn reboot(&self) -> Result<(), togi_hardware::RebootError> {
            panic!("notify does not reboot")
        }

        fn stderr_facts(&self) -> OutputFacts {
            OutputFacts::default()
        }

        fn stdout_facts(&self) -> OutputFacts {
            OutputFacts::default()
        }

        fn day_observer<'lock>(
            &self,
            _: &'lock HostLock,
        ) -> Result<Box<dyn togi_hardware::day::Observer + 'lock>, togi_machine::Error> {
            Err(togi_machine::Error::Other(
                "test host has no day observer".into(),
            ))
        }

        fn open_machine<'lock>(
            &self,
            _: &togi_config::Config,
            _: &Path,
            _: &'lock HostLock,
        ) -> Result<togi_machine::Machine<'lock>, togi_hardware::Error> {
            panic!("notify does not open the machine")
        }

        fn day_request(&self, request: &Request) -> Result<Reply, SocketError> {
            self.requests.borrow_mut().push(match request {
                Request::Questions => Seen::Questions,
                Request::Answer { boot, answer } => Seen::Answer(boot.clone(), *answer),
            });
            let mut replies = self.replies.borrow_mut();
            let reply = replies.pop_front().expect("unscripted day request");
            if replies.is_empty()
                && let Some(token) = &*self.token.borrow()
            {
                token.cancel();
            }
            reply
        }

        fn day_notification_actions(&self) -> bool {
            self.actions
        }

        fn day_notification(&self, invocation: &Invocation) -> Result<Output, togi_command::Error> {
            self.shown.borrow_mut().push(invocation.clone());
            self.notifications
                .borrow_mut()
                .pop_front()
                .expect("unscripted notification")
        }

        fn interactive(&self) -> bool {
            false
        }

        fn stderr(&self) -> Box<dyn std::io::Write> {
            Box::new(std::io::sink())
        }

        fn surface(&self) -> Arc<dyn Surface> {
            panic!("notify draws no dashboard")
        }
    }

    /// Each `sleep` pops the next scripted result, as an injected clock must not wait ten seconds.
    struct Ticks {
        results: VecDeque<Result<(), togi_machine::Error>>,
        slept: Vec<Duration>,
    }

    impl Ticks {
        fn new(results: impl IntoIterator<Item = Result<(), togi_machine::Error>>) -> Self {
            Self {
                results: results.into_iter().collect(),
                slept: Vec::new(),
            }
        }
    }

    impl Clock for Ticks {
        fn now(&self) -> jiff::Timestamp {
            Timestamp::UNIX_EPOCH
        }

        fn monotonic(&self) -> Result<Duration, togi_machine::Error> {
            Ok(Duration::ZERO)
        }

        fn sleep(
            &mut self,
            _: &CancellationToken,
            duration: Duration,
        ) -> Result<(), togi_machine::Error> {
            self.slept.push(duration);
            self.results.pop_front().expect("unscripted sleep")
        }
    }

    fn end(boot: &str) -> DayUncleanEnd {
        DayUncleanEnd {
            boot: boot.to_owned().into(),
            last_checkpoint: None,
            offsets: togi_machine::PerCore::default(),
            reset_reason: togi_machine::ResetReason {
                kind: None,
                raw: Vec::new(),
                supported: false,
            },
            evidence: togi_machine::EndEvidence::unknown(),
        }
    }

    fn pending(boots: &[&str]) -> Reply {
        Reply::Questions {
            pending: boots.iter().map(|boot| end(boot)).collect(),
            morning: None,
        }
    }

    fn chose(stdout: &[u8]) -> Output {
        Output {
            status: ExitStatus::from_raw(0),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    fn drive(desktop: &Desktop, ticks: &mut Ticks) -> (Exit, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = {
            let mut ctx = Context {
                state_dir: Path::new("/state"),
                out: &mut out,
                err: &mut err,
                system: desktop,
            };
            serve(&mut ctx, &CancellationToken::new(), ticks)
        };
        assert_eq!(out, b"", "notify writes nothing to stdout");
        (exit, String::from_utf8(err).unwrap())
    }

    #[test]
    fn a_morning_report_is_notified_once_per_boot_without_question_actions() {
        let report = crate::morning::Report {
            boot: boot("morning"),
            text: "No automatic nonzero change; restricted to stock (CO 0).".into(),
        };
        let reply = || Reply::Questions {
            pending: Vec::new(),
            morning: Some(report.clone()),
        };
        let desktop = Desktop::new([Ok(reply()), Ok(reply())], [Ok(chose(b"frozen\n"))]);
        let mut ticks = Ticks::new([Ok(()), Err(togi_machine::Error::Cancelled)]);
        let (exit, err) = drive(&desktop, &mut ticks);
        assert_eq!((exit, err.as_str()), (Exit::Ok, ""));
        assert_eq!(*desktop.shown.borrow(), [morning_invocation(&report)]);
        assert_eq!(
            *desktop.requests.borrow(),
            [Seen::Questions, Seen::Questions]
        );
    }
    fn boot(id: &str) -> BootId {
        id.to_owned().into()
    }

    #[test_case(true ; "plasma with actions")]
    #[test_case(false ; "other desktop without actions")]
    fn a_pending_question_raises_one_notification_and_dismissal_sends_no_answer(actions: bool) {
        let desktop = Desktop {
            actions,
            ..Desktop::new([Ok(pending(&["crashed"]))], [Ok(chose(b""))])
        };
        let mut ticks = Ticks::new([Err(togi_machine::Error::Cancelled)]);

        let (exit, err) = drive(&desktop, &mut ticks);

        assert_eq!((exit, err.as_str()), (Exit::Ok, ""));
        assert_eq!(
            *desktop.shown.borrow(),
            [invocation(&end("crashed"), actions)]
        );
        assert_eq!(*desktop.requests.borrow(), [Seen::Questions]);
        assert_eq!(ticks.slept, [Duration::from_secs(10)]);
    }

    #[test_case(b"frozen\n", DayAnswerKind::Frozen)]
    #[test_case(b"graphics\n", DayAnswerKind::Graphics)]
    #[test_case(b"power-off\n", DayAnswerKind::PowerOff)]
    #[test_case(b"unsure\n", DayAnswerKind::Unsure)]
    fn a_chosen_action_answers_the_notified_boot(stdout: &[u8], answer: DayAnswerKind) {
        let desktop = Desktop {
            actions: true,
            ..Desktop::new(
                [
                    Ok(pending(&["other-pending", "crashed"])),
                    Ok(Reply::Answered),
                ],
                [Ok(chose(b"")), Ok(chose(stdout))],
            )
        };
        let mut ticks = Ticks::new([Err(togi_machine::Error::Cancelled)]);

        let (exit, err) = drive(&desktop, &mut ticks);

        assert_eq!((exit, err.as_str()), (Exit::Ok, ""));
        assert_eq!(
            *desktop.requests.borrow(),
            [Seen::Questions, Seen::Answer(boot("crashed"), answer)],
        );
        assert_eq!(desktop.shown.borrow().len(), 2);
    }

    #[test]
    fn an_already_notified_question_is_not_notified_again_but_a_new_one_is() {
        let desktop = Desktop::new(
            [
                Ok(pending(&["first"])),
                Ok(pending(&["first"])),
                Ok(pending(&["first", "second"])),
            ],
            [Ok(chose(b"")), Ok(chose(b""))],
        );
        let mut ticks = Ticks::new([Ok(()), Ok(()), Err(togi_machine::Error::Cancelled)]);

        let (exit, err) = drive(&desktop, &mut ticks);

        assert_eq!((exit, err.as_str()), (Exit::Ok, ""));
        assert_eq!(
            *desktop.shown.borrow(),
            [
                invocation(&end("first"), false),
                invocation(&end("second"), false)
            ],
        );
        assert_eq!(
            *desktop.requests.borrow(),
            [Seen::Questions, Seen::Questions, Seen::Questions],
        );
        assert_eq!(ticks.slept, [Duration::from_secs(10); 3]);
    }

    #[test]
    fn a_dismissed_question_is_not_asked_again_even_when_it_stays_pending() {
        let desktop = Desktop::new(
            [Ok(pending(&["first"])), Ok(pending(&["first"]))],
            [Ok(chose(b""))],
        );
        let mut ticks = Ticks::new([Ok(()), Err(togi_machine::Error::Cancelled)]);

        let (exit, _) = drive(&desktop, &mut ticks);

        assert_eq!(exit, Exit::Ok);
        assert_eq!(desktop.shown.borrow().len(), 1);
        assert!(
            desktop
                .requests
                .borrow()
                .iter()
                .all(|seen| *seen == Seen::Questions)
        );
    }

    #[test]
    fn a_day_service_failure_ends_the_loop_before_any_notification() {
        let desktop = Desktop::new([Err(SocketError::Refused("no access".into()))], []);
        let mut ticks = Ticks::new([]);

        let (exit, err) = drive(&desktop, &mut ticks);

        assert_eq!(
            (exit, err.as_str()),
            (
                Exit::Error,
                "togi day notify: day service refused the request: no access\n"
            ),
        );
        assert!(desktop.shown.borrow().is_empty());
        assert!(ticks.slept.is_empty());
    }

    #[test_case(Ok(Reply::Answered) ; "answered")]
    #[test_case(Ok(Reply::Refused { reason: "no".into() }) ; "refused")]
    fn a_wrong_reply_to_the_question_list_ends_the_loop(reply: Result<Reply, SocketError>) {
        let desktop = Desktop::new([reply], []);
        let mut ticks = Ticks::new([]);

        let (exit, err) = drive(&desktop, &mut ticks);

        assert_eq!(
            (exit, err.as_str()),
            (
                Exit::Error,
                "togi day notify: unexpected day service reply\n"
            ),
        );
    }

    #[test]
    fn a_failed_notify_send_ends_the_loop_without_an_answer() {
        let desktop = Desktop::new(
            [Ok(pending(&["crashed"]))],
            [Ok(Output {
                status: ExitStatus::from_raw(1 << 8),
                stdout: b"frozen\n".to_vec(),
                stderr: Vec::new(),
            })],
        );
        let mut ticks = Ticks::new([]);

        let (exit, err) = drive(&desktop, &mut ticks);

        assert_eq!(
            (exit, err.as_str()),
            (Exit::Error, "togi day notify: notify-send failed\n"),
        );
        assert_eq!(*desktop.requests.borrow(), [Seen::Questions]);
        assert!(ticks.slept.is_empty());
    }

    #[test]
    fn an_unstartable_notify_send_ends_the_loop() {
        let desktop = Desktop::new(
            [Ok(pending(&["crashed"]))],
            [Err(togi_command::Error::Spawn {
                program: "notify-send".into(),
                source: std::io::Error::other("not installed"),
            })],
        );
        let mut ticks = Ticks::new([]);

        let (exit, err) = drive(&desktop, &mut ticks);

        assert_eq!(
            (exit, err.as_str()),
            (
                Exit::Error,
                "togi day notify: cannot start notify-send: not installed\n"
            ),
        );
    }

    #[test_case(Err(SocketError::Refused("not yours".into())), "day service refused the request: not yours" ; "failed request")]
    #[test_case(Ok(pending(&[])), "unexpected day service reply" ; "wrong reply")]
    #[test_case(Ok(Reply::Refused { reason: "no".into() }), "unexpected day service reply" ; "refusal reply")]
    fn a_failed_answer_ends_the_loop(reply: Result<Reply, SocketError>, message: &str) {
        let desktop = Desktop::new([Ok(pending(&["crashed"])), reply], [Ok(chose(b"frozen\n"))]);
        let mut ticks = Ticks::new([]);

        let (exit, err) = drive(&desktop, &mut ticks);

        assert_eq!(
            (exit, err),
            (Exit::Error, format!("togi day notify: {message}\n")),
        );
        assert_eq!(
            *desktop.requests.borrow(),
            [
                Seen::Questions,
                Seen::Answer(boot("crashed"), DayAnswerKind::Frozen)
            ],
        );
        assert!(ticks.slept.is_empty());
    }

    #[test]
    fn a_clock_failure_ends_the_loop() {
        let desktop = Desktop::new([Ok(pending(&[]))], []);
        let mut ticks = Ticks::new([Err(togi_machine::Error::Crashed)]);

        let (exit, err) = drive(&desktop, &mut ticks);

        assert_eq!(
            (exit, err.as_str()),
            (
                Exit::Error,
                "togi day notify: the simulated machine crashed\n"
            ),
        );
    }

    #[test]
    fn run_listens_for_signals_and_stops_cleanly_once_cancelled() {
        let desktop = Desktop::new(
            [Ok(pending(&["crashed"])), Ok(Reply::Answered)],
            [Ok(chose(b"graphics\n"))],
        );
        let (mut out, mut err) = (Vec::new(), Vec::new());

        let exit = run(&mut Context {
            state_dir: Path::new("/state"),
            out: &mut out,
            err: &mut err,
            system: &desktop,
        });

        assert_eq!(exit, Exit::Ok);
        assert_eq!(
            *desktop.requests.borrow(),
            [
                Seen::Questions,
                Seen::Answer(boot("crashed"), DayAnswerKind::Graphics)
            ],
        );
        assert_eq!((out, err), (Vec::new(), Vec::new()));
    }

    #[test]
    fn run_reports_a_signal_listener_that_cannot_start_and_asks_nothing() {
        let desktop = Desktop::default();
        *desktop.listen_error.borrow_mut() = Some(crate::signals::Error::Lost);
        let (mut out, mut err) = (Vec::new(), Vec::new());

        let exit = run(&mut Context {
            state_dir: Path::new("/state"),
            out: &mut out,
            err: &mut err,
            system: &desktop,
        });

        assert_eq!(exit, Exit::Error);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "togi day notify: the signal listener stopped\n",
        );
        assert!(desktop.requests.borrow().is_empty());
    }
}
