//! `togi events`: the journal, one readable line per event.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::sync::LazyLock;

use clap::builder::{PossibleValue, TypedValueParser};
use clap::{Arg, Args as ClapArgs, Command, Error, error::ErrorKind};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::{Event, Filter, KindSelector, TrialId};
use togi_machine::{CoreId, CoreSet};
use togi_render::{OutputFacts, Renderer};

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::read;
use crate::report::fail;

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[
        ("togi events --core 3", "Everything that happened to core 3"),
        (
            "togi events --kind trial,checking.cycle",
            "Every trial event and checking cycle",
        ),
        (
            "togi events --json --trial 0413",
            "The raw events of trial 0413",
        ),
    ])
});

/// Print the journal, one readable line per event
///
/// Oldest first. Filters combine, so you can narrow the journal to one core, one trial or a time
/// window. A different ruleset warns before rendering, including `--json`; a different schema is
/// refused. A kind selector must name a known exact kind or group; an unknown name and an
/// explicitly empty list are usage errors (exit 2). A valid filter with no matches prints nothing
/// and exits 0. Readable lines are colored on terminals and in the system journal unless `NO_COLOR`
/// is set.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    /// Only events naming core N, and the outcomes of trials that load it
    #[arg(long, value_name = "N", value_parser = parse_core, value_hint = clap::ValueHint::Other)]
    core: Option<CoreId>,

    /// Only these comma-separated known kinds, or groups such as trial for every trial.* kind
    #[arg(long, value_name = "kinds", value_parser = KindList)]
    kind: Vec<Vec<KindSelector>>,

    /// Only events of trial ID
    #[arg(long, value_name = "ID", value_hint = clap::ValueHint::Other)]
    trial: Option<String>,

    /// Only events at or after this RFC 3339 time
    #[arg(long, value_name = "time", value_parser = parse_time, value_hint = clap::ValueHint::Other)]
    since: Option<Timestamp>,

    /// Only events before this RFC 3339 time
    #[arg(long, value_name = "time", value_parser = parse_time, value_hint = clap::ValueHint::Other)]
    until: Option<Timestamp>,

    /// Print raw, uncolored JSON events
    #[arg(long)]
    json: bool,
}

fn parse_core(text: &str) -> Result<CoreId, String> {
    let value: i64 = text
        .parse()
        .map_err(|_| "must be a core number".to_owned())?;
    CoreId::new(value).map_err(|error| error.to_string())
}

fn parse_time(text: &str) -> Result<Timestamp, String> {
    text.parse()
        .map_err(|_| "must be an RFC 3339 time".to_owned())
}

#[derive(Clone)]
struct KindList;

impl TypedValueParser for KindList {
    type Value = Vec<KindSelector>;

    fn parse_ref(
        &self,
        cmd: &Command,
        arg: Option<&Arg>,
        value: &OsStr,
    ) -> Result<Self::Value, Error> {
        let fail = |message: String| {
            let arg = arg.map_or_else(|| "--kind".to_owned(), ToString::to_string);
            let value = value.to_string_lossy();
            Error::raw(
                ErrorKind::ValueValidation,
                format!("invalid value '{value}' for '{arg}': {message}\n"),
            )
            .with_cmd(cmd)
        };
        let text = value
            .to_str()
            .ok_or_else(|| fail("not valid UTF-8".to_owned()))?;
        let mut selectors = Vec::new();
        for name in text
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            selectors.push(
                name.parse::<KindSelector>()
                    .map_err(|e| fail(e.to_string()))?,
            );
        }
        if selectors.is_empty() {
            return Err(fail(togi_journal::InvalidKindSelector::Empty.to_string()));
        }
        Ok(selectors)
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        Some(Box::new(
            KindSelector::names().into_iter().map(PossibleValue::new),
        ))
    }
}

/// Runs `togi events`.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    let facts = OutputFacts::detect(
        &io::stdout(),
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("JOURNAL_STREAM").as_deref(),
    );
    let zone = ctx.system.zone();
    run_with(args, ctx, facts, &zone)
}

fn run_with(args: &Args, ctx: &mut Context<'_>, facts: OutputFacts, zone: &TimeZone) -> Exit {
    let recording = match read::open(ctx, "events") {
        Ok(recording) => recording,
        Err(exit) => return exit,
    };
    let filter = Filter {
        cores: args.core.into_iter().collect::<CoreSet>(),
        kinds: args.kind.iter().flatten().copied().collect(),
        trial: args.trial.clone().map(TrialId::from),
        since: args.since,
        until: args.until,
    };
    let renderer = Renderer::new(facts);
    for event in filter.select(recording.events()) {
        if let Err(error) = write_event(ctx.out, renderer, event, zone, args.json) {
            return fail(ctx, "events", format!("write output: {error}"));
        }
    }
    if let Err(error) = ctx.out.flush() {
        return fail(ctx, "events", format!("write output: {error}"));
    }
    read::report(ctx, "events", &recording);
    Exit::Ok
}

fn write_event(
    out: &mut dyn Write,
    renderer: Renderer,
    event: &Event,
    zone: &TimeZone,
    json: bool,
) -> io::Result<()> {
    if json {
        serde_json::to_writer(&mut *out, event)?;
        writeln!(out)
    } else {
        renderer.write_event(out, event, zone)
    }
}
