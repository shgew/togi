//! `togi day record`: journal a crash no day observer saw.
//!
//! The command gathers a [`Draft`] from flags, asks at a terminal for whatever the flags leave out, and hands it
//! to `togi-session`, which checks it against the journal. Flags and answers are one grammar, so the same
//! description gives the same record either way.

use std::io;

use clap::Args as ClapArgs;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::{DayCrashes, Journal, Payload, RecordableEnd, Running};
use togi_machine::{BootId, CcdId, CoreId};
use togi_session::{Crash, Draft, Entered, OffsetSpec, append_record, draft_record};

use crate::cli::Context;
use crate::exit::Exit;
use crate::report::{current_build, emit, exit_with};
use crate::system::SystemClock;

const COMMAND: &str = "day record";

/// Record a crash togi did not observe
///
/// Journals a crash from before the day service existed, or on a machine without it, so Ruleset 13
/// can treat its offsets as a whole-profile veto. At a terminal it asks for what the options leave
/// out, with defaults; anywhere else every part without a default comes from an option. It prints
/// what the record means before appending it, and asks first at a terminal.
///
/// A crash is --boot, a boot with a recorded unclean end, or --unobserved-at, a crash with none.
/// Offsets without --offset are the ones the day observer read back for that boot. Offsets from
/// --offset are entered offsets and need --entered-at. Give every core of the session a value:
/// all=-20, ccd1=-25 or core3=-10, where a more specific one overrides a less specific one.
///
/// Needs root and a session journal. It holds the writer lock: stop the day service first. Times are
/// RFC 3339 (2026-10-19T14:30:00Z) or local (2026-10-19 14:30).
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// The crashed boot: the ID of a boot with a recorded unclean end
    #[arg(long, value_name = "BOOT", conflicts_with = "unobserved_at")]
    boot: Option<String>,

    /// When a crash happened that has no recorded unclean end
    #[arg(long, value_name = "TIME")]
    unobserved_at: Option<String>,

    /// An offset the crashed boot ran: all=N, ccdI=N or coreI=N; repeat for more
    #[arg(long = "offset", value_name = "SPEC")]
    offsets: Vec<String>,

    /// When the offsets given with --offset were entered in firmware setup
    #[arg(long, value_name = "TIME")]
    entered_at: Option<String>,

    /// What was running: idle, desktop, build, game, other or unknown
    #[arg(long, value_name = "CATEGORY", value_parser = parse_running)]
    running: Option<Running>,

    /// Free text about what was running
    #[arg(long, value_name = "TEXT")]
    note: Option<String>,

    /// Append without asking first at a terminal
    #[arg(long)]
    yes: bool,
}

fn parse_running(value: &str) -> Result<Running, String> {
    Running::ALL
        .into_iter()
        .find(|running| running.as_str() == value)
        .ok_or_else(|| format!("expected one of {}", categories()))
}

fn categories() -> String {
    Running::ALL
        .into_iter()
        .map(Running::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("give {0}")]
    Missing(&'static str),
    #[error("{flag}: {reason}")]
    Invalid { flag: &'static str, reason: String },
    #[error("--entered-at applies only to offsets given with --offset")]
    EnteredWithoutOffsets,
    #[error("the input ended before every question was answered")]
    InputEnded,
    #[error("read an answer")]
    Read(#[from] io::Error),
}

impl Error {
    const fn exit(&self) -> Exit {
        match self {
            Self::Missing(_) | Self::Invalid { .. } | Self::EnteredWithoutOffsets => Exit::Usage,
            Self::InputEnded | Self::Read(_) => Exit::Error,
        }
    }
}

/// Whoever answers the questions: a terminal, or nobody.
trait Ask {
    /// Writes `text` to the user.
    fn say(&mut self, text: &str);

    /// Asks `question` and returns the line typed without its newline, or `None` when the input ended.
    fn ask(&mut self, question: &str) -> io::Result<Option<String>>;
}

struct Terminal<'a, 'b> {
    ctx: &'a mut Context<'b>,
}

impl Ask for Terminal<'_, '_> {
    fn say(&mut self, text: &str) {
        let _ = writeln!(self.ctx.err, "{text}");
    }

    fn ask(&mut self, question: &str) -> io::Result<Option<String>> {
        write!(self.ctx.err, "{question} ")?;
        self.ctx.err.flush()?;
        Ok(self
            .ctx
            .system
            .prompt_line()?
            .map(|line| line.trim().to_owned()))
    }
}

/// Asks until `parse` accepts the answer. An empty answer takes `default` when there is one.
fn ask_until<T>(
    asker: &mut dyn Ask,
    question: &str,
    default: Option<T>,
    parse: impl Fn(&str) -> Result<T, String>,
) -> Result<T, Error> {
    let mut default = default;
    loop {
        let line = asker.ask(question)?.ok_or(Error::InputEnded)?;
        if line.is_empty()
            && let Some(default) = default.take()
        {
            return Ok(default);
        }
        match parse(&line) {
            Ok(value) => return Ok(value),
            Err(reason) => asker.say(&reason),
        }
    }
}

/// Parses an RFC 3339 time, or a local date and time such as `2026-10-19 14:30`.
fn parse_time(text: &str, zone: &TimeZone) -> Result<Timestamp, String> {
    if let Ok(time) = text.parse::<Timestamp>() {
        return Ok(time);
    }
    let local = text.replacen(' ', "T", 1);
    local
        .parse::<jiff::civil::DateTime>()
        .map_err(|_| {
            format!("{text:?} is not a time like 2026-10-19T14:30:00Z or 2026-10-19 14:30")
        })
        .and_then(|civil| {
            zone.to_timestamp(civil)
                .map_err(|error| format!("{text:?}: {error}"))
        })
}

/// Parses `all=N`, `ccdI=N` or `coreI=N`.
fn parse_spec(text: &str) -> Result<OffsetSpec, String> {
    let (key, value) = text
        .split_once('=')
        .ok_or_else(|| format!("{text:?} is not like all=-20, ccd1=-25 or core3=-10"))?;
    let value = value
        .parse::<i64>()
        .map_err(|_| format!("{text:?}: {value:?} is not a number"))?;
    let index = |digits: &str| {
        digits
            .parse::<i64>()
            .map_err(|_| format!("{text:?}: {digits:?} is not a number"))
    };
    if key == "all" {
        Ok(OffsetSpec::All(value))
    } else if let Some(digits) = key.strip_prefix("ccd") {
        let ccd = CcdId::new(index(digits)?).map_err(|error| format!("{text:?}: {error}"))?;
        Ok(OffsetSpec::Ccd(ccd, value))
    } else if let Some(digits) = key.strip_prefix("core") {
        let core = CoreId::new(index(digits)?).map_err(|error| format!("{text:?}: {error}"))?;
        Ok(OffsetSpec::Core(core, value))
    } else {
        Err(format!(
            "{text:?} is not like all=-20, ccd1=-25 or core3=-10"
        ))
    }
}

fn parse_specs(text: &str) -> Result<Vec<OffsetSpec>, String> {
    let specs = text
        .split([' ', ','])
        .filter(|token| !token.is_empty())
        .map(parse_spec)
        .collect::<Result<Vec<_>, _>>()?;
    if specs.is_empty() {
        return Err("give at least one offset, like all=-20".to_owned());
    }
    Ok(specs)
}

/// What the flags gave, parsed.
struct Given {
    boot: Option<BootId>,
    unobserved_at: Option<Timestamp>,
    specs: Vec<OffsetSpec>,
    entered_at: Option<Timestamp>,
    running: Option<Running>,
    note: Option<String>,
}

impl Given {
    fn of(args: &Args, zone: &TimeZone) -> Result<Self, Error> {
        let time = |flag: &'static str, text: &Option<String>| {
            text.as_deref()
                .map(|text| {
                    parse_time(text, zone).map_err(|reason| Error::Invalid { flag, reason })
                })
                .transpose()
        };
        let mut specs = Vec::new();
        for text in &args.offsets {
            specs.push(parse_spec(text).map_err(|reason| Error::Invalid {
                flag: "--offset",
                reason,
            })?);
        }
        if args.entered_at.is_some() && specs.is_empty() {
            return Err(Error::EnteredWithoutOffsets);
        }
        Ok(Self {
            boot: args.boot.clone().map(BootId::from),
            unobserved_at: time("--unobserved-at", &args.unobserved_at)?,
            specs,
            entered_at: time("--entered-at", &args.entered_at)?,
            running: args.running,
            note: args.note.clone(),
        })
    }
}

fn offsets_text(end: &RecordableEnd) -> String {
    let parts: Vec<_> = end
        .end
        .offsets
        .iter()
        .map(|(core, offset)| format!("{core:02}: {}", offset.0))
        .collect();
    format!("[{}]", parts.join(", "))
}

/// The unclean ends as a numbered list, and the default choice: the newest without a record.
fn list_ends(crashes: &DayCrashes, asker: &mut dyn Ask) -> Option<usize> {
    if crashes.ends().is_empty() {
        asker.say("The journal holds no unclean end of a day observer's boot.");
        return None;
    }
    asker.say("Unclean ends the day observer recorded, oldest first:");
    for (index, named) in crashes.ends().iter().enumerate() {
        let read = named.read_back.map_or_else(
            || "never read back".to_owned(),
            |at| format!("last read back {at}"),
        );
        let recorded = named
            .recorded
            .map_or_else(String::new, |seq| format!(" (already recorded, seq {seq})"));
        asker.say(&format!(
            "  {}  {}  {}  {}{recorded}",
            index + 1,
            named.end.boot,
            read,
            offsets_text(named),
        ));
    }
    crashes.newest_unrecorded().and_then(|newest| {
        crashes
            .ends()
            .iter()
            .position(|named| named.end.boot == newest.end.boot)
    })
}

/// Which crash: a boot of the list, or one no observer saw.
enum Choice {
    Boot(BootId),
    Unobserved,
}

fn parse_choice(text: &str, crashes: &DayCrashes) -> Result<Choice, String> {
    if text.eq_ignore_ascii_case("none") {
        return Ok(Choice::Unobserved);
    }
    if let Ok(number) = text.parse::<usize>() {
        return number
            .checked_sub(1)
            .and_then(|index| crashes.ends().get(index))
            .map(|named| Choice::Boot(named.end.boot.clone()))
            .ok_or_else(|| format!("{text} is not one of the numbers listed"));
    }
    crashes
        .ends()
        .iter()
        .find(|named| named.end.boot.as_str() == text)
        .map(|named| Choice::Boot(named.end.boot.clone()))
        .ok_or_else(|| format!("{text:?} is not a listed boot; type its number, its ID or none"))
}

fn ask_crash(
    crashes: &DayCrashes,
    asker: &mut dyn Ask,
    zone: &TimeZone,
) -> Result<CrashKind, Error> {
    let default = list_ends(crashes, asker);
    let (question, default_choice) = match default
        .and_then(|index| crashes.ends().get(index).map(|named| (index, named)))
    {
        Some((index, named)) => (
            format!(
                "Which boot crashed? Type its number or ID, or none for a crash no observer saw [{}]:",
                index + 1
            ),
            Choice::Boot(named.end.boot.clone()),
        ),
        None => (
            "Which boot crashed? Type its number or ID, or none [none]:".to_owned(),
            Choice::Unobserved,
        ),
    };
    match ask_until(asker, &question, Some(default_choice), |text| {
        parse_choice(text, crashes)
    })? {
        Choice::Boot(boot) => Ok(CrashKind::Observed(boot)),
        Choice::Unobserved => ask_until(
            asker,
            &format!("When did it crash? ({TIME_FORMS}):"),
            None,
            |text| parse_time(text, zone),
        )
        .map(CrashKind::Unobserved),
    }
}

enum CrashKind {
    Observed(BootId),
    Unobserved(Timestamp),
}

/// Stands in for the user when there is no terminal; every part it would be asked for comes from the flags.
struct Quiet;

impl Ask for Quiet {
    fn say(&mut self, _: &str) {}

    fn ask(&mut self, _: &str) -> io::Result<Option<String>> {
        Ok(None)
    }
}

const SPECS_QUESTION: &str = "Offsets, as all=-20 ccd1=-25 core3=-10 (every core needs one):";
const TIME_FORMS: &str = "2026-10-19T14:30:00Z or 2026-10-19 14:30";

/// The offsets the user types instead of the observer's reading, if any. `readback` is the reading a boot of the
/// journal has, which an empty answer keeps.
fn offsets_from(
    given: &Given,
    kind: &CrashKind,
    crashes: &DayCrashes,
    interactive: bool,
    asker: &mut dyn Ask,
) -> Result<Option<Vec<OffsetSpec>>, Error> {
    if !given.specs.is_empty() {
        return Ok(Some(given.specs.clone()));
    }
    match kind {
        CrashKind::Observed(boot) => {
            let Some(named) = crashes.get(boot).filter(|_| interactive) else {
                return Ok(None);
            };
            let question = format!(
                "Offsets the observer read back, {}. Press Enter to record them, or type the offsets that crashed:",
                offsets_text(named)
            );
            let line = asker.ask(&question)?.ok_or(Error::InputEnded)?;
            if line.is_empty() {
                return Ok(None);
            }
            match parse_specs(&line) {
                Ok(specs) => Ok(Some(specs)),
                Err(reason) => {
                    asker.say(&reason);
                    ask_until(asker, SPECS_QUESTION, None, parse_specs).map(Some)
                }
            }
        }
        CrashKind::Unobserved(_) if interactive => {
            ask_until(asker, SPECS_QUESTION, None, parse_specs).map(Some)
        }
        CrashKind::Unobserved(_) => Err(Error::Missing("--offset")),
    }
}

/// The draft of a record, from the flags and then, at a terminal, the user. Without one, a part the flags leave
/// out and no default covers is an error.
fn collect(
    given: &Given,
    crashes: &DayCrashes,
    interactive: bool,
    asker: &mut dyn Ask,
    zone: &TimeZone,
) -> Result<Draft, Error> {
    let kind = match (&given.boot, given.unobserved_at) {
        (Some(boot), _) => CrashKind::Observed(boot.clone()),
        (None, Some(at)) => CrashKind::Unobserved(at),
        (None, None) if interactive => ask_crash(crashes, asker, zone)?,
        (None, None) => return Err(Error::Missing("--boot or --unobserved-at")),
    };
    let typed = offsets_from(given, &kind, crashes, interactive, asker)?;
    let entered = match typed {
        None => None,
        Some(specs) => {
            let at = match given.entered_at {
                Some(at) => at,
                None if interactive => ask_until(
                    asker,
                    &format!("When did you enter these offsets in firmware setup? ({TIME_FORMS}):"),
                    None,
                    |text| parse_time(text, zone),
                )?,
                None => return Err(Error::Missing("--entered-at")),
            };
            Some(Entered { at, specs })
        }
    };
    let crash = match kind {
        CrashKind::Observed(boot) => Crash::Observed { boot, entered },
        CrashKind::Unobserved(at) => match entered {
            Some(entered) => Crash::Unobserved { at, entered },
            None => return Err(Error::Missing("--offset")),
        },
    };
    let running = match given.running {
        Some(running) => running,
        None if interactive => ask_until(
            asker,
            &format!("What was running? {} [unknown]:", categories()),
            Some(Running::Unknown),
            parse_running,
        )?,
        None => Running::Unknown,
    };
    let note = match &given.note {
        Some(note) => Some(note.clone()),
        None if interactive => {
            let line = asker
                .ask("A note about what was running (optional):")?
                .ok_or(Error::InputEnded)?;
            (!line.is_empty()).then_some(line)
        }
        None => None,
    };
    Ok(Draft {
        crash,
        running,
        note,
    })
}

/// Records a crash under the state directory's writer lock.
pub(super) fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    if !ctx.system.privileged() {
        return exit_with(ctx, COMMAND, "recording a crash needs root", Exit::Error);
    }
    let zone = ctx.system.zone();
    let given = match Given::of(args, &zone) {
        Ok(given) => given,
        Err(error) => {
            let exit = error.exit();
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), exit);
        }
    };
    let build = current_build();
    let boot = match ctx.system.boot_id() {
        Ok(boot) => boot,
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
        }
    };
    let mut journal = match crate::day_destination::open(ctx, boot, &build) {
        Ok(journal) => journal,
        Err(exit) => return exit,
    };
    let exit = record(args, &given, &zone, ctx, &mut journal);
    match journal.close() {
        Ok(_dir) => exit,
        Err(error) => exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error),
    }
}

fn record(
    args: &Args,
    given: &Given,
    zone: &TimeZone,
    ctx: &mut Context<'_>,
    journal: &mut Journal,
) -> Exit {
    let interactive = ctx.system.interactive();
    let crashes = DayCrashes::of(journal.events());
    let drafted = if interactive {
        collect(
            given,
            &crashes,
            true,
            &mut Terminal { ctx: &mut *ctx },
            zone,
        )
    } else {
        collect(given, &crashes, false, &mut Quiet, zone)
    };
    let draft = match drafted {
        Ok(draft) => draft,
        Err(error) => {
            let exit = error.exit();
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), exit);
        }
    };
    let record = match draft_record(journal.events(), &draft) {
        Ok(record) => record,
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
        }
    };
    let described = Payload::DayRecord(record.clone()).message();
    if let Err(exit) = emit(ctx, format_args!("{described}\n{}", record.effect())) {
        return exit;
    }
    if interactive && !args.yes {
        let confirmed = Terminal { ctx: &mut *ctx }.ask("Append this record? [Y/n]");
        match confirmed {
            Ok(Some(line))
                if line.is_empty()
                    || line.eq_ignore_ascii_case("y")
                    || line.eq_ignore_ascii_case("yes") => {}
            Ok(Some(_) | None) => return exit_with(ctx, COMMAND, "nothing recorded", Exit::Error),
            Err(error) => {
                return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
            }
        }
    }
    match append_record(journal, &SystemClock(ctx.system), record) {
        Ok(seq) => emit(ctx, format_args!("crash recorded as event {seq}"))
            .map_or_else(|exit| exit, |()| Exit::Ok),
        Err(error) => exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error),
    }
}

#[cfg(test)]
mod tests;
