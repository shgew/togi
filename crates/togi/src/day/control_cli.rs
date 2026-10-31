//! Unprivileged day controls; the observer alone owns persistence and hardware transactions.

use std::time::Duration;

use clap::Args as ClapArgs;
use jiff::{Timestamp, ToSpan as _, civil::Time, tz::TimeZone};
use togi_journal::PauseState;

use super::socket::{FreezeRequest, PauseRequest, Reply, Request};
use crate::cli::Context;
use crate::exit::Exit;
use crate::report::{emit, exit_with};

/// Pause at stock until the next cold boot, for a duration, or until the next local HH:MM.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Pause for a positive duration, such as 30m, 2h or 1h30m; omitted means until cold boot
    #[arg(value_parser = parse_duration, conflicts_with = "until")]
    duration: Option<Duration>,
    /// Pause until the next occurrence of this local time (HH:MM)
    #[arg(long, value_name = "HH:MM", value_parser = parse_until)]
    until: Option<Time>,
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let mut rest = value;
    let mut seconds = 0_u64;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return Err("expected a positive duration such as 30m, 2h or 1h30m".into());
        }
        let number: u64 = rest[..digits]
            .parse()
            .map_err(|_| "duration is too large")?;
        rest = &rest[digits..];
        let (unit, scale) = match rest.as_bytes().first() {
            Some(b's') => (1, 1),
            Some(b'm') => (1, 60),
            Some(b'h') => (1, 3600),
            Some(b'd') => (1, 86400),
            _ => return Err("each duration component needs s, m, h or d".into()),
        };
        seconds = number
            .checked_mul(scale)
            .and_then(|part| seconds.checked_add(part))
            .ok_or("duration is too large")?;
        rest = &rest[unit..];
    }
    if seconds == 0 {
        return Err("duration must be positive".into());
    }
    Ok(Duration::from_secs(seconds))
}

fn parse_until(value: &str) -> Result<Time, String> {
    if value.len() != 5
        || value.as_bytes()[2] != b':'
        || !value
            .bytes()
            .enumerate()
            .all(|(i, byte)| i == 2 || byte.is_ascii_digit())
    {
        return Err("expected local time HH:MM".into());
    }
    value
        .parse()
        .map_err(|_| "expected local time HH:MM in 00:00..23:59".into())
}

fn state(args: &Args, now: Timestamp, zone: &TimeZone) -> Result<PauseState, jiff::Error> {
    if let Some(duration) = args.duration {
        return Ok(PauseState::Until {
            deadline: now.checked_add(duration)?,
        });
    }
    if let Some(time) = args.until {
        let mut date = zone.to_datetime(now).date();
        loop {
            let local = date.to_datetime(time);
            let earlier = zone.to_ambiguous_timestamp(local).earlier()?;
            if earlier > now && zone.to_datetime(earlier) == local {
                return Ok(PauseState::Until { deadline: earlier });
            }
            let later = zone.to_ambiguous_timestamp(local).later()?;
            if later > now && zone.to_datetime(later) == local {
                return Ok(PauseState::Until { deadline: later });
            }
            // A skipped local time has no occurrence today; a repeated time has two.
            date = date.checked_add(1.day())?;
        }
    }
    Ok(PauseState::UntilBoot {})
}

pub(super) fn pause(args: &Args, ctx: &mut Context<'_>) -> Exit {
    let state = match state(args, ctx.system.now(), &ctx.system.zone()) {
        Ok(state) => state,
        Err(error) => {
            return exit_with(
                ctx,
                "day pause",
                togi_render::error_chain(&error),
                Exit::Error,
            );
        }
    };
    control(
        ctx,
        &Request::Pause(PauseRequest { state }),
        "day pause",
        "day paused at verified stock",
    )
}

pub(super) fn resume(ctx: &mut Context<'_>) -> Exit {
    control(
        ctx,
        &Request::Resume,
        "day resume",
        "committed profile reapplied and verified; day resumed",
    )
}

pub(super) fn freeze(frozen: bool, ctx: &mut Context<'_>) -> Exit {
    control(
        ctx,
        &Request::Freeze(FreezeRequest { frozen }),
        if frozen { "day freeze" } else { "day unfreeze" },
        if frozen {
            "day frozen; pause lifetime unchanged"
        } else {
            "day unfrozen; pause lifetime unchanged"
        },
    )
}

fn control(ctx: &mut Context<'_>, request: &Request, command: &str, message: &str) -> Exit {
    match ctx.system.day_request(request) {
        Ok(Reply::Controlled { .. }) => {
            emit(ctx, format_args!("{message}")).map_or_else(|exit| exit, |()| Exit::Ok)
        }
        Ok(
            Reply::Questions { .. }
            | Reply::State { .. }
            | Reply::Answered
            | Reply::Recorded { .. }
            | Reply::Reapplied
            | Reply::Refused { .. },
        ) => exit_with(ctx, command, "unexpected day service reply", Exit::Error),
        Err(error) => exit_with(ctx, command, togi_render::error_chain(&error), Exit::Error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use test_case::test_case;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    #[test_case("30m", 1800)]
    #[test_case("2h", 7200)]
    #[test_case("1h30m", 5400)]
    #[test_case("1d", 86400)]
    fn durations_are_checked_seconds(value: &str, seconds: u64) {
        assert_eq!(parse_duration(value).unwrap(), Duration::from_secs(seconds));
    }

    #[test_case("")]
    #[test_case("0m")]
    #[test_case("-1h")]
    #[test_case("1")]
    #[test_case("18446744073709551615d")]
    fn malformed_or_overflowing_durations_are_refused(value: &str) {
        assert!(parse_duration(value).is_err());
    }

    #[test]
    fn duration_and_until_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["pause", "1h", "--until", "12:30"]).is_err());
    }

    #[test_case("24:00")]
    #[test_case("1:00")]
    #[test_case("12:30:00")]
    fn until_is_strict_local_hhmm(value: &str) {
        assert!(parse_until(value).is_err());
    }

    #[test_case("2026-10-29T10:00:00Z", "12:30", "2026-10-29T12:30:00Z")]
    #[test_case("2026-10-29T12:30:00Z", "12:30", "2026-10-30T12:30:00Z")]
    #[test_case("2026-10-29T23:00:00Z", "00:30", "2026-10-30T00:30:00Z")]
    fn until_uses_the_next_occurrence(now: &str, until: &str, expected: &str) {
        let args = Args {
            duration: None,
            until: Some(parse_until(until).unwrap()),
        };
        assert_eq!(
            state(&args, now.parse().unwrap(), &TimeZone::UTC).unwrap(),
            PauseState::Until {
                deadline: expected.parse().unwrap()
            }
        );
    }

    #[test]
    fn no_duration_means_until_cold_boot() {
        let args = Args {
            duration: None,
            until: None,
        };
        assert_eq!(
            state(&args, Timestamp::UNIX_EPOCH, &TimeZone::UTC).unwrap(),
            PauseState::UntilBoot {}
        );
    }

    #[test]
    fn until_is_local_not_utc() {
        let args = Args {
            duration: None,
            until: Some(parse_until("12:30").unwrap()),
        };
        assert_eq!(
            state(
                &args,
                "2026-10-29T10:00:00Z".parse().unwrap(),
                &TimeZone::fixed(jiff::tz::offset(2))
            )
            .unwrap(),
            PauseState::Until {
                deadline: "2026-10-29T10:30:00Z".parse().unwrap()
            }
        );
    }

    #[test_case("2026-11-01T05:45:00Z", "01:30", "2026-11-01T06:30:00Z"; "second occurrence in a repeated hour")]
    #[test_case("2026-03-08T06:00:00Z", "02:30", "2026-03-09T06:30:00Z"; "skip a nonexistent local time")]
    fn until_follows_actual_local_occurrences(now: &str, until: &str, expected: &str) {
        let args = Args {
            duration: None,
            until: Some(parse_until(until).unwrap()),
        };
        let zone = TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(
            state(&args, now.parse().unwrap(), &zone).unwrap(),
            PauseState::Until {
                deadline: expected.parse().unwrap()
            }
        );
    }
}
