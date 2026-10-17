//! The manifest: every phrase literal of the dashboard's renderers, read from their own source, and the cases that
//! rendered each.
//!
//! A literal is a string literal outside the test modules and outside panics, assertions, attributes and the
//! corpus hooks. Its fragments are what stays of it when the `{}` placeholders and line breaks are cut out; the
//! literal is *rendered* when one recorded phrase carries all of its fragments, so a template is proved by a
//! concrete value filling it, not by its text appearing somewhere. A fragment without a letter ties nothing and
//! is dropped; a literal with no word of three letters is no phrase and is not listed.
//!
//! The evidence is substring evidence: it shows a template branch ran, not that no other branch renders the same
//! words. [`SILENT`] lists the literals that render nothing of their own, with the reason, and the exporter fails
//! on any other literal no case rendered, and on a silent entry that matches no literal.

use std::sync::LazyLock;

use super::Collector;

/// The renderer sources, relative to this file, each paired with the path the document shows. Their inline test
/// modules are cut off.
const SOURCES: &[(&str, &str)] = &[
    (
        "frames/story/mod.rs",
        include_str!("../frames/story/mod.rs"),
    ),
    (
        "frames/story/outcome.rs",
        include_str!("../frames/story/outcome.rs"),
    ),
    (
        "frames/story/resting.rs",
        include_str!("../frames/story/resting.rs"),
    ),
    ("frames/terms.rs", include_str!("../frames/terms.rs")),
    (
        "frames/dash/header.rs",
        include_str!("../frames/dash/header.rs"),
    ),
    (
        "frames/dash/stage.rs",
        include_str!("../frames/dash/stage.rs"),
    ),
    ("frames/dash/now.rs", include_str!("../frames/dash/now.rs")),
    ("frames/dash/ccd.rs", include_str!("../frames/dash/ccd.rs")),
    ("frames/dash/r7.rs", include_str!("../frames/dash/r7.rs")),
    ("frames/notes.rs", include_str!("../frames/notes.rs")),
    (
        "frames/panels/pages.rs",
        include_str!("../frames/panels/pages.rs"),
    ),
    (
        "frames/panels/cycle.rs",
        include_str!("../frames/panels/cycle.rs"),
    ),
    (
        "frames/panels/history.rs",
        include_str!("../frames/panels/history.rs"),
    ),
    (
        "frames/panels/hunt.rs",
        include_str!("../frames/panels/hunt.rs"),
    ),
    (
        "frames/panels/log.rs",
        include_str!("../frames/panels/log.rs"),
    ),
    (
        "frames/panels/search.rs",
        include_str!("../frames/panels/search.rs"),
    ),
    ("frames/footer.rs", include_str!("../frames/footer.rs")),
    ("frames/help.rs", include_str!("../frames/help.rs")),
    ("frames/render.rs", include_str!("../frames/render.rs")),
    (
        "frames/main_page.rs",
        include_str!("../frames/main_page.rs"),
    ),
    ("frames/scene.rs", include_str!("../frames/scene.rs")),
    ("frames/draw.rs", include_str!("../frames/draw.rs")),
    ("words.rs", include_str!("../words.rs")),
    ("describe.rs", include_str!("../describe.rs")),
    ("history.rs", include_str!("../history.rs")),
];

/// Statement text before a literal that makes it no phrase.
const NOT_PHRASES: &[&str] = &[
    "panic!(",
    "expect(",
    "assert",
    "unreachable!(",
    "#[",
    "todo!(",
    "reason =",
    "corpus::",
    "include_str",
];

/// Why the outcome-phrase columns render nothing: since the dashboard redesign no production code reads them.
const UNREAD_COLUMNS: &str = "read only through `Words::detailed`, `Words::compact`, `decision_detail` or `decision_brief`, which no production code reads since the dashboard redesign: the history and the story word these events themselves";

/// Why `hunt_end_phrase` renders nothing.
const UNREAD_HUNT_END: &str = "`hunt_end_phrase` has no production caller: `HuntEnd`'s history is contextual and `describe.rs` words a hunt's end itself";

/// Why the story never tells of a bracketed singleton.
const UNREACHED_BRACKET: &str = "`hunt_story` words a singleton only while its probe trial runs, and the tuner marks a singleton's probe done only when its probing ends, which ends the hunt";

/// Literals no case can render, and why. The document lists these with their reason first, even when another
/// phrase happens to carry their words, and the exporter does not fail on them. Each holds because of the code
/// the reason names; an entry matching no literal fails the exporter ([`stale`]).
pub(crate) const SILENT: &[(&str, &str, &str)] = &[
    (
        "words.rs",
        "idle with short bursts",
        "`regime_words(R6)` has no production caller: `load_words` words R6 itself",
    ),
    (
        "words.rs",
        "core {core:02} starts its search",
        UNREAD_COLUMNS,
    ),
    ("words.rs", "the hunt ends", UNREAD_HUNT_END),
    ("words.rs", "hunt {hunt} ends: ", UNREAD_HUNT_END),
    (
        "words.rs",
        "hunt {hunt} keeps {cores} together",
        UNREAD_HUNT_END,
    ),
    (
        "words.rs",
        "hunt {hunt} keeps the failure on loaded",
        UNREAD_HUNT_END,
    ),
    (
        "words.rs",
        "core {core:02} starts confirming",
        UNREAD_COLUMNS,
    ),
    ("words.rs", "core {core:02} deepens", UNREAD_COLUMNS),
    ("words.rs", "core {core:02} failure held at", UNREAD_COLUMNS),
    ("words.rs", "CCD {} partial on", UNREAD_COLUMNS),
    ("words.rs", "deepening round {round} starts", UNREAD_COLUMNS),
    ("words.rs", "deepening round {round} ends", UNREAD_COLUMNS),
    ("words.rs", "tuning stops: ", UNREAD_COLUMNS),
    ("words.rs", "hunt {} starts over", UNREAD_COLUMNS),
    ("words.rs", "hunt ends", UNREAD_COLUMNS),
    ("words.rs", "no hunt", UNREAD_COLUMNS),
    (
        "frames/panels/hunt.rs",
        "→ core {id:02} failed alone; testing its parked offset",
        "`HuntView::singleton` stages a probe `Anchor` only when its parked offset is the probe's offset, so an anchor without a parked offset never reaches `group_lines`",
    ),
    (
        "frames/story/mod.rs",
        "A pass sits one count shallower",
        UNREACHED_BRACKET,
    ),
    (
        "frames/story/mod.rs",
        "Core {id:02} is bracketed",
        UNREACHED_BRACKET,
    ),
];

/// One phrase literal of the source.
#[derive(Debug)]
pub(crate) struct Literal {
    /// The source file, relative to `crates/togi-watch/src`.
    pub(crate) file: &'static str,
    /// Its line.
    pub(crate) line: usize,
    /// The literal as written, escapes decoded.
    pub(crate) text: String,
    /// What a rendered phrase must carry of it, each trimmed.
    pub(crate) fragments: Vec<String>,
}

impl Literal {
    /// Why no case renders it, when [`SILENT`] says so.
    pub(crate) fn silent(&self) -> Option<&'static str> {
        SILENT
            .iter()
            .find(|(file, start, _)| *file == self.file && self.text.starts_with(start))
            .map(|(_, _, reason)| *reason)
    }
}

/// The [`SILENT`] entries that match no literal, as `file: start`: the source moved on and the entry excuses
/// nothing. The exporter fails on any.
pub(crate) fn stale() -> Vec<String> {
    SILENT
        .iter()
        .filter(|(file, start, _)| {
            !literals()
                .iter()
                .any(|literal| literal.file == *file && literal.text.starts_with(start))
        })
        .map(|(file, start, _)| format!("{file}: {start:?}"))
        .collect()
}

/// Every phrase literal of the renderers, in source order.
pub(crate) fn literals() -> &'static [Literal] {
    static LITERALS: LazyLock<Vec<Literal>> = LazyLock::new(|| {
        let mut out = Vec::new();
        for &(file, source) in SOURCES {
            for (line, text) in extract(production(source)) {
                let fragments = fragments(&text);
                let phrase = fragments
                    .iter()
                    .any(|fragment| fragment.chars().filter(|c| c.is_alphabetic()).count() >= 3);
                if phrase {
                    out.push(Literal {
                        file,
                        line,
                        text,
                        fragments,
                    });
                }
            }
        }
        out
    });
    &LITERALS
}

/// The literals `text` carries: every fragment of each is in it.
pub(crate) fn carried(text: &str) -> Vec<usize> {
    literals()
        .iter()
        .enumerate()
        .filter(|(_, literal)| {
            literal
                .fragments
                .iter()
                .all(|fragment| text.contains(fragment.as_str()))
        })
        .map(|(at, _)| at)
        .collect()
}

/// The literals no case rendered and [`SILENT`] does not excuse, as `file:line: text`.
pub(crate) fn uncovered(collector: &Collector) -> Vec<String> {
    literals()
        .iter()
        .zip(&collector.literals)
        .filter(|(literal, cases)| !cases.any() && literal.silent().is_none())
        .map(|(literal, _)| format!("{}:{}: {:?}", literal.file, literal.line, literal.text))
        .collect()
}

/// The source up to its inline test module.
fn production(source: &str) -> &str {
    let mut from = 0;
    while let Some(found) = source[from..].find("#[cfg(test)]\nmod ") {
        let at = from + found;
        let header = source[at + "#[cfg(test)]\n".len()..]
            .lines()
            .next()
            .unwrap_or_default();
        if header.trim_end().ends_with('{') {
            return &source[..at];
        }
        from = at + 1;
    }
    source
}

/// What of `text` a phrase must carry: the pieces between placeholders and line breaks that have a letter.
fn fragments(text: &str) -> Vec<String> {
    let text = text.replace("{{", "\u{1}").replace("}}", "\u{2}");
    let mut out = Vec::new();
    let mut piece = String::new();
    let mut chars = text.chars();
    let mut flush = |piece: &mut String| {
        let kept = piece.replace('\u{1}', "{").replace('\u{2}', "}");
        let kept = kept.trim();
        if kept.chars().any(char::is_alphabetic) {
            out.push(kept.to_owned());
        }
        piece.clear();
    };
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                for inner in chars.by_ref() {
                    if inner == '}' {
                        break;
                    }
                }
                flush(&mut piece);
            }
            '\n' => flush(&mut piece),
            c => piece.push(c),
        }
    }
    flush(&mut piece);
    out
}

/// The string literals of `source` that are phrases, with their lines and escapes decoded.
fn extract(source: &str) -> Vec<(usize, String)> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut line = 1;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = source[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |at| i + 2 + at + 2);
                line += source[i..end].matches('\n').count();
                i = end;
            }
            b'\'' => i = skip_char_literal(source, i),
            b'r' | b'b' if raw_string(bytes, i).is_some() => {
                let (hashes, open) = raw_string(bytes, i).unwrap_or_default();
                let close = format!("\"{}", "#".repeat(hashes));
                let end = source[open..]
                    .find(&close)
                    .map_or(bytes.len(), |at| open + at);
                let text = &source[open..end];
                if is_phrase(bytes, i) {
                    out.push((line, text.to_owned()));
                }
                line += text.matches('\n').count();
                i = end + close.len();
            }
            b'"' => {
                let start = i;
                let (text, end) = decode(source, i + 1);
                if is_phrase(bytes, start) {
                    out.push((line, text));
                }
                line += source[start..end].matches('\n').count();
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

/// After the char literal at `i`, or `i + 1` when the quote opens a lifetime.
fn skip_char_literal(source: &str, i: usize) -> usize {
    let bytes = source.as_bytes();
    if bytes.get(i + 1) == Some(&b'\\') {
        let mut j = i + 3;
        while j < bytes.len() && bytes[j] != b'\'' {
            j += 1;
        }
        return j + 1;
    }
    match source[i + 1..].chars().next() {
        Some(c) if bytes.get(i + 1 + c.len_utf8()) == Some(&b'\'') => i + 2 + c.len_utf8(),
        _ => i + 1,
    }
}

/// For `r"`, `r#"`, `br"` at `i` that start a token: the number of `#` and where the text begins.
fn raw_string(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
        return None;
    }
    let mut j = i;
    if bytes[j] == b'b' {
        j += 1;
    }
    if bytes.get(j) != Some(&b'r') {
        return None;
    }
    j += 1;
    let hashes = bytes[j..].iter().take_while(|&&b| b == b'#').count();
    j += hashes;
    (bytes.get(j) == Some(&b'"')).then_some((hashes, j + 1))
}

/// The decoded text of the string whose body starts at `from`, and where its closing quote ends.
fn decode(source: &str, from: usize) -> (String, usize) {
    let bytes = source.as_bytes();
    let mut text = String::new();
    let mut j = from;
    while j < bytes.len() && bytes[j] != b'"' {
        if bytes[j] != b'\\' {
            let c = source[j..].chars().next().unwrap_or_default();
            text.push(c);
            j += c.len_utf8();
            continue;
        }
        match bytes.get(j + 1) {
            Some(b'n') => {
                text.push('\n');
                j += 2;
            }
            Some(b't') => {
                text.push('\t');
                j += 2;
            }
            Some(b'r') => {
                text.push('\r');
                j += 2;
            }
            Some(b'0') => {
                text.push('\0');
                j += 2;
            }
            Some(b'u') => {
                let close = source[j..].find('}').map_or(bytes.len(), |at| j + at);
                let code = u32::from_str_radix(&source[j + 3..close], 16).unwrap_or(0xFFFD);
                text.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                j = close + 1;
            }
            Some(b'\n') => {
                j += 2;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
            }
            Some(_) => {
                let c = source[j + 1..].chars().next().unwrap_or_default();
                text.push(c);
                j += 1 + c.len_utf8();
            }
            None => j += 1,
        }
    }
    (text, j + 1)
}

/// Whether the literal at `at` is a phrase: the statement it sits in holds none of [`NOT_PHRASES`].
fn is_phrase(bytes: &[u8], at: usize) -> bool {
    let before = &bytes[at.saturating_sub(240)..at];
    let mut cut = 0;
    for (i, &b) in before.iter().enumerate() {
        let ends_line = |from: usize| {
            before[from..]
                .iter()
                .take_while(|&&c| c != b'\n')
                .all(u8::is_ascii_whitespace)
                && before[from..].contains(&b'\n')
        };
        if b == b';' || ((b == b'{' || b == b'}') && ends_line(i + 1)) {
            cut = i + 1;
        }
    }
    let statement = String::from_utf8_lossy(&before[cut..]);
    !NOT_PHRASES.iter().any(|skip| statement.contains(skip))
}
