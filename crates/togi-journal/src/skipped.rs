//! What a lenient read left out, for the reader to say.

use std::fmt;

use crate::event::Seq;

/// An event of a kind this build does not know, passed over whole.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkippedEvent {
    pub line: u64,
    pub seq: Seq,
    /// The kind as the journal spells it.
    pub kind: String,
}

/// The unknown fields one event carried and a lenient read ignored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IgnoredFields {
    pub line: u64,
    pub seq: Seq,
    /// The kind as the journal spells it.
    pub kind: String,
    /// Where each field sat in the line, for example `data.foo`; `foo` for an envelope field.
    pub fields: Vec<String>,
}

/// Everything a lenient read skipped or ignored, in line order. Its [`Display`](fmt::Display) is one line to print
/// next to what the read produced; an empty report prints nothing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Skipped {
    events: Vec<SkippedEvent>,
    ignored: Vec<IgnoredFields>,
}

impl Skipped {
    pub(crate) const fn new() -> Self {
        Self {
            events: Vec::new(),
            ignored: Vec::new(),
        }
    }

    pub(crate) fn skip(&mut self, line: u64, seq: Seq, kind: String) {
        self.events.push(SkippedEvent { line, seq, kind });
    }

    pub(crate) fn ignore(&mut self, line: u64, seq: Seq, kind: &str, fields: Vec<String>) {
        if !fields.is_empty() {
            self.ignored.push(IgnoredFields {
                line,
                seq,
                kind: kind.to_owned(),
                fields,
            });
        }
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.ignored.is_empty()
    }

    /// The events passed over, in line order.
    pub fn events(&self) -> &[SkippedEvent] {
        &self.events
    }

    /// The unknown fields ignored in events that were read, in line order.
    pub fn ignored(&self) -> &[IgnoredFields] {
        &self.ignored
    }
}

/// Journal text, plain when it is made of visible ASCII and quoted with escapes otherwise.
struct Text<'a>(&'a str);

impl fmt::Display for Text<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.chars().all(|c| c.is_ascii_graphic()) {
            f.write_str(self.0)
        } else {
            write!(f, "{:?}", self.0)
        }
    }
}

fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

fn write_lines(f: &mut fmt::Formatter<'_>, lines: &[u64]) -> fmt::Result {
    write!(f, "{} ", plural(lines.len(), "line", "lines"))?;
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{line}")?;
    }
    Ok(())
}

impl fmt::Display for Skipped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.events.is_empty() {
            let mut kinds: Vec<(&str, Vec<u64>)> = Vec::new();
            for event in &self.events {
                match kinds.iter_mut().find(|(kind, _)| *kind == event.kind) {
                    Some((_, lines)) => lines.push(event.line),
                    None => kinds.push((&event.kind, vec![event.line])),
                }
            }
            write!(
                f,
                "skipped {} {} this build does not know (",
                self.events.len(),
                plural(self.events.len(), "event of a kind", "events of kinds"),
            )?;
            for (index, (kind, lines)) in kinds.iter().enumerate() {
                if index > 0 {
                    f.write_str("; ")?;
                }
                write!(f, "{} at ", Text(kind))?;
                write_lines(f, lines)?;
            }
            f.write_str(")")?;
        }
        if !self.ignored.is_empty() {
            if !self.events.is_empty() {
                f.write_str("; ")?;
            }
            f.write_str("ignored unknown fields: ")?;
            for (index, ignored) in self.ignored.iter().enumerate() {
                if index > 0 {
                    f.write_str("; ")?;
                }
                write!(f, "{}", Text(&ignored.kind))?;
                for (position, field) in ignored.fields.iter().enumerate() {
                    f.write_str(if position == 0 { " " } else { ", " })?;
                    write!(f, "{}", Text(field))?;
                }
                write!(f, " (line {})", ignored.line)?;
            }
        }
        Ok(())
    }
}
