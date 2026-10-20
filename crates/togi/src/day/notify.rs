//! Desktop notification adapter; the durable question remains in the journal after dismissal.

use std::collections::BTreeSet;
use std::time::Duration;

use togi_command::Invocation;
use togi_journal::{DayAnswerKind, DayUncleanEnd};
use togi_machine::{CancellationToken, Clock as _};

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
    let mut sent = BTreeSet::new();
    loop {
        let pending = match ctx.system.day_request(&Request::Questions) {
            Ok(Reply::Questions { pending }) => pending,
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
        match clock.sleep(&cancel, Duration::from_secs(10)) {
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
}
