use std::fmt;
use std::io;

use jiff::tz::TimeZone;
use togi_journal::Event;

use crate::escape::escape_text;
use crate::output::OutputFacts;
use crate::style::{Style, style_for};

const RESET: &str = "\x1b[0m";
const JOURNALD_ERR: &str = "<3>";
const KIND_WIDTH: usize = 14;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Renderer {
    color: bool,
    journald: bool,
}

impl Renderer {
    #[must_use]
    pub const fn new(facts: OutputFacts) -> Self {
        Self {
            color: !facts.no_color && (facts.is_terminal || facts.journald),
            journald: facts.journald,
        }
    }

    #[must_use]
    pub const fn line<'a>(&self, event: &'a Event, zone: &'a TimeZone) -> EventLine<'a> {
        EventLine {
            renderer: *self,
            event,
            zone,
            prefix: "",
        }
    }

    #[must_use]
    pub const fn prefixed_line<'a>(
        &self,
        event: &'a Event,
        zone: &'a TimeZone,
        prefix: &'a str,
    ) -> EventLine<'a> {
        EventLine {
            renderer: *self,
            event,
            zone,
            prefix,
        }
    }

    #[must_use]
    pub const fn text<'a>(&self, style: Style, text: &'a str) -> StyledText<'a> {
        StyledText {
            renderer: *self,
            style,
            text,
        }
    }

    pub fn write_event<W: io::Write + ?Sized>(
        &self,
        out: &mut W,
        event: &Event,
        zone: &TimeZone,
    ) -> io::Result<()> {
        writeln!(out, "{}", self.line(event, zone))
    }

    fn decorate(
        self,
        f: &mut fmt::Formatter<'_>,
        style: Style,
        body: &dyn fmt::Display,
    ) -> fmt::Result {
        if self.journald && style.is_error() {
            f.write_str(JOURNALD_ERR)?;
        }
        match style.sgr().filter(|_| self.color) {
            Some(sgr) => write!(f, "{sgr}{body}{RESET}"),
            None => write!(f, "{body}"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EventLine<'a> {
    renderer: Renderer,
    event: &'a Event,
    zone: &'a TimeZone,
    prefix: &'a str,
}

struct Plain<'a>(&'a EventLine<'a>);

impl fmt::Display for Plain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let line = self.0;
        let time = line.zone.to_datetime(line.event.time);
        write!(
            f,
            "{}{:02}:{:02}:{:02} {:<KIND_WIDTH$} {}",
            escape_text(line.prefix),
            time.hour(),
            time.minute(),
            time.second(),
            line.event.kind().as_str(),
            escape_text(&line.event.msg),
        )
    }
}

impl fmt::Display for EventLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.renderer
            .decorate(f, style_for(&self.event.payload), &Plain(self))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StyledText<'a> {
    renderer: Renderer,
    style: Style,
    text: &'a str,
}

impl fmt::Display for StyledText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.renderer
            .decorate(f, self.style, &escape_text(self.text))
    }
}
