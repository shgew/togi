//! Text fitting: every dashboard line is measured as the screen will show it.
//!
//! Text from the journal is untrusted, so everything passes through [`inert`]
//! first: control characters become visible escapes
//! ([`togi_render::escape_text`]). On the console profiles glyphs the console
//! font cannot draw then become ASCII or a `\u` escape ([`console_text`]);
//! [`Colours::Rich`] draws Unicode as it is, except that characters joining or
//! restyling a neighbour into one glyph (variation selectors, zero-width
//! joiners, skin-tone modifiers, regional indicators, emoji tags) become a `\u`
//! escape too, since terminals disagree on the width of such sequences. Widths
//! are display cells of that escaped text (double-width characters take two),
//! so a line never overflows because of what it escapes to.
//!
//! A line that does not fit never cuts inside a value or a word: it first
//! shortens or leaves out whole clauses ([`fit_clauses`]), lowest priority
//! first, and text that must still be cut keeps the words that fit and ends
//! with the profile's [`cut_marker`] ([`cut_words`]).

use std::borrow::Cow;
use std::fmt::Write as _;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use togi_render::escape_text;
use unicode_width::UnicodeWidthChar as _;

use crate::screen::Colours;

/// Ends text cut to fit on the console profiles; the console font has no `…`.
const CONSOLE_CUT_MARKER: &str = "...";

/// Ends text cut to fit: three ASCII dots on the console profiles, one `…` cell on [`Colours::Rich`].
#[must_use]
pub const fn cut_marker(colours: Colours) -> &'static str {
    if rich(colours) {
        "…"
    } else {
        CONSOLE_CUT_MARKER
    }
}

/// Whether `colours` draws Unicode as it is instead of the console's character set.
const fn rich(colours: Colours) -> bool {
    matches!(colours, Colours::Rich)
}

/// Characters a cut leaves dangling at the end of the kept words.
const DANGLING: &str = " ·→,;:";

/// The glyphs of IBM437, the character set of the kernel's built-in console fonts, beyond ASCII.
const IBM437: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■ ☺☻♥♦♣♠•◘○◙♂♀♪♫☼►◄↕‼¶§▬↨↑↓→←∟↔▲▼";

/// Whether the console font draws `c`.
#[must_use]
pub fn is_console_glyph(c: char) -> bool {
    c.is_ascii() || IBM437.contains(c)
}

/// Replaces what the console font cannot draw: common typography by ASCII, anything else by a visible `\u`
/// escape.
#[must_use]
fn console_text(text: &str) -> Cow<'_, str> {
    if text.chars().all(is_console_glyph) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            c if is_console_glyph(c) => out.push(c),
            '×' => out.push('x'),
            '…' => out.push_str(CONSOLE_CUT_MARKER),
            '–' | '—' => out.push('-'),
            '‘' | '’' => out.push('\''),
            '“' | '”' => out.push('"'),
            c => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
        }
    }
    Cow::Owned(out)
}

/// `text` with the glyphs `colours` cannot draw replaced as [`console_text`] does; unchanged on [`Colours::Rich`].
/// Does not escape controls; [`inert`] does, at the point of measuring.
#[must_use]
pub fn glyph_text(text: &str, colours: Colours) -> Cow<'_, str> {
    if rich(colours) {
        Cow::Borrowed(text)
    } else {
        console_text(text)
    }
}

/// Whether `c` joins or restyles a neighbouring character into one glyph whose width terminals disagree on:
/// variation selectors, the zero-width joiner, emoji skin-tone modifiers, regional indicators and emoji tags.
const fn joins_graphemes(c: char) -> bool {
    matches!(
        c,
        '\u{200d}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{1f1e6}'..='\u{1f1ff}'
            | '\u{1f3fb}'..='\u{1f3ff}'
            | '\u{e0020}'..='\u{e007f}'
            | '\u{e0100}'..='\u{e01ef}'
    )
}

/// `text` as the screen may show it: untrusted control characters escaped, then, on the console profiles, what the
/// console cannot draw, and on [`Colours::Rich`] the characters that join graphemes ([`joins_graphemes`]), so
/// each character left takes the cells its own width says. Idempotent, so escaping twice changes nothing.
#[must_use]
pub fn inert(text: &str, colours: Colours) -> Cow<'_, str> {
    if rich(colours) {
        let escaped = escape_text(text).into_cow();
        if !escaped.chars().any(joins_graphemes) {
            return escaped;
        }
        let mut out = String::with_capacity(escaped.len() + 8);
        for c in escaped.chars() {
            if joins_graphemes(c) {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            } else {
                out.push(c);
            }
        }
        return Cow::Owned(out);
    }
    if text
        .chars()
        .all(|c| c == ' ' || c.is_ascii_graphic() || (!c.is_ascii() && is_console_glyph(c)))
    {
        return Cow::Borrowed(text);
    }
    let escaped = escape_text(text).to_string();
    match console_text(&escaped) {
        Cow::Borrowed(_) => Cow::Owned(escaped),
        Cow::Owned(console) => Cow::Owned(console),
    }
}

fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// The display width of `text` once escaped for the screen.
#[must_use]
pub fn display_width(text: &str, colours: Colours) -> usize {
    inert(text, colours).chars().map(char_width).sum()
}

/// The display width of `line` once escaped for the screen.
#[must_use]
pub fn line_width(line: &Line<'_>, colours: Colours) -> usize {
    line.spans
        .iter()
        .map(|span| display_width(&span.content, colours))
        .sum()
}

/// `line` as plain text, as the screen shows it.
#[must_use]
pub fn plain(line: &Line<'_>, colours: Colours) -> String {
    line.spans
        .iter()
        .map(|span| inert(&span.content, colours))
        .collect()
}

type Cell = (char, Style);

fn cells_of(line: &Line<'_>, colours: Colours) -> Vec<Cell> {
    let mut out = Vec::new();
    for span in &line.spans {
        let style = line.style.patch(span.style);
        out.extend(inert(&span.content, colours).chars().map(|c| (c, style)));
    }
    out
}

fn cells_width(cells: &[Cell]) -> usize {
    cells.iter().map(|&(c, _)| char_width(c)).sum()
}

fn line_of(cells: &[Cell]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_style = Style::new();
    for &(c, style) in cells {
        if !run.is_empty() && style != run_style {
            spans.push(Span::styled(std::mem::take(&mut run), run_style));
        }
        run_style = style;
        run.push(c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style));
    }
    Line::from(spans)
}

/// How many leading cells fit in `room` display cells.
fn prefix_len(cells: &[Cell], room: usize) -> usize {
    let mut used = 0;
    for (i, &(c, _)) in cells.iter().enumerate() {
        used += char_width(c);
        if used > room {
            return i;
        }
    }
    cells.len()
}

/// `line` with `style` under every span: what a span sets itself wins.
#[must_use]
pub fn tint(line: Line<'static>, style: Style) -> Line<'static> {
    let spans = line
        .spans
        .into_iter()
        .map(|span| Span::styled(span.content, style.patch(span.style)))
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// `line` cut by display cells to `width`, escaped for the screen. For gauges and rule dashes, which are clipped
/// by cell; text uses [`cut_styled`].
#[must_use]
pub fn truncate_styled(line: &Line<'_>, width: usize, colours: Colours) -> Line<'static> {
    let cells = cells_of(line, colours);
    line_of(&cells[..prefix_len(&cells, width)])
}

/// `line` fitted to `width` display cells as the screen will show it. Text that does not fit keeps the whole
/// words that fit and ends with the [`cut_marker`] in the style of the last kept word, or is left out when no
/// word fits.
#[must_use]
pub fn cut_styled(line: &Line<'_>, width: usize, colours: Colours) -> Line<'static> {
    let mut cells = cells_of(line, colours);
    if width == 0 {
        return Line::default();
    }
    if cells_width(&cells) <= width {
        return line_of(&cells);
    }
    let marker = cut_marker(colours);
    let Some(room) = width.checked_sub(marker.chars().map(char_width).sum()) else {
        return Line::default();
    };
    let mut keep = prefix_len(&cells, room);
    if cells.get(keep).is_none_or(|&(c, _)| c != ' ') {
        keep = cells[..keep]
            .iter()
            .rposition(|&(c, _)| c == ' ')
            .unwrap_or(0);
    }
    while keep > 0 && DANGLING.contains(cells[keep - 1].0) {
        keep -= 1;
    }
    if keep == 0 {
        return Line::default();
    }
    let style = cells[keep - 1].1;
    cells.truncate(keep);
    cells.extend(marker.chars().map(|c| (c, style)));
    line_of(&cells)
}

/// `text` fitted to `width` display cells; see [`cut_styled`].
#[must_use]
pub fn cut_words(text: &str, width: usize, colours: Colours) -> String {
    plain(&cut_styled(&Line::from(text), width, colours), colours)
}

/// One way a clause reads. `rank` is how early the clause leaves this form for the next one when the line does
/// not fit: the higher rank goes first, and of equal ranks the later clause.
#[derive(Debug, Clone)]
pub struct Form {
    /// The text in this form; empty to leave the clause out.
    pub text: Line<'static>,
    /// How early the clause gives this form up.
    pub rank: u8,
}

impl Form {
    /// A form that reads `text`.
    #[must_use]
    pub fn new(text: impl Into<Line<'static>>, rank: u8) -> Self {
        Self {
            text: text.into(),
            rank,
        }
    }

    /// The last form of a clause the line may leave out.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            text: Line::default(),
            rank: 0,
        }
    }
}

/// A part of a line that shows whole in one of its forms, from longest to shortest. An empty last form lets the
/// line leave the clause out; a value has none.
#[derive(Debug, Clone)]
pub struct Clause {
    /// What goes before the clause unless it opens the line.
    pub sep: Line<'static>,
    /// The forms, longest first.
    pub forms: Vec<Form>,
}

impl Clause {
    /// A clause with these forms, after `sep`.
    #[must_use]
    pub fn new(sep: impl Into<Line<'static>>, forms: Vec<Form>) -> Self {
        assert!(!forms.is_empty(), "a clause has at least one form");
        Self {
            sep: sep.into(),
            forms,
        }
    }

    /// A clause with one form, never shortened or left out.
    #[must_use]
    pub fn whole(sep: impl Into<Line<'static>>, text: impl Into<Line<'static>>) -> Self {
        Self::new(sep, vec![Form::new(text, 0)])
    }
}

/// Joins `clauses` into `width` display cells, shortening or leaving out the clauses the caller ranks lowest
/// until the line fits; once none has another form, [`cut_styled`] cuts the line.
#[must_use]
pub fn fit_clauses(width: usize, clauses: &[Clause], colours: Colours) -> Line<'static> {
    let mut at = vec![0; clauses.len()];
    loop {
        let mut line: Vec<Cell> = Vec::new();
        for (clause, &form) in clauses.iter().zip(&at) {
            let text = cells_of(&clause.forms[form].text, colours);
            if text.is_empty() {
                continue;
            }
            if !line.is_empty() {
                line.extend(cells_of(&clause.sep, colours));
            }
            line.extend(text);
        }
        let mut next: Option<usize> = None;
        for (i, clause) in clauses.iter().enumerate() {
            let shortens = at[i] + 1 < clause.forms.len();
            let rank = |at: &[usize], i: usize| clauses[i].forms[at[i]].rank;
            if shortens && next.is_none_or(|n| rank(&at, i) >= rank(&at, n)) {
                next = Some(i);
            }
        }
        match next {
            Some(next) if cells_width(&line) > width => at[next] += 1,
            _ => return cut_styled(&line_of(&line), width, colours),
        }
    }
}

/// `text` wrapped to `width` display cells in `style`; see [`wrap_line`].
#[must_use]
pub fn wrap_styled(text: &str, width: usize, style: Style, colours: Colours) -> Vec<Line<'static>> {
    wrap_line(&Line::from(Span::styled(text, style)), width, colours)
}

/// `line` wrapped to `width` display cells, every word in its own style and every gap in the style of the space
/// it replaces. A newline starts a paragraph; a word wider than the width is broken by cells, and a character
/// wider than the width is left out.
#[must_use]
pub(crate) fn wrap_line(line: &Line<'_>, width: usize, colours: Colours) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let base = line
        .spans
        .first()
        .map_or(line.style, |span| line.style.patch(span.style));
    let mut paragraphs: Vec<Vec<Cell>> = vec![Vec::new()];
    for span in &line.spans {
        let style = line.style.patch(span.style);
        for (i, piece) in span.content.split('\n').enumerate() {
            if i > 0 {
                paragraphs.push(Vec::new());
            }
            if let Some(paragraph) = paragraphs.last_mut() {
                paragraph.extend(inert(piece, colours).chars().map(|c| (c, style)));
            }
        }
    }
    let mut out = Vec::new();
    let push = |out: &mut Vec<Line<'static>>, cells: &[Cell]| {
        out.push(if cells.is_empty() {
            Line::from(Span::styled(String::new(), base))
        } else {
            line_of(cells)
        });
    };
    for paragraph in &paragraphs {
        let mut row: Vec<Cell> = Vec::new();
        let mut rest = paragraph.as_slice();
        while let Some(start) = rest.iter().position(|&(c, _)| !c.is_whitespace()) {
            let gap = rest[..start].first().map(|&(_, style)| style);
            rest = &rest[start..];
            let end = rest
                .iter()
                .position(|&(c, _)| c.is_whitespace())
                .unwrap_or(rest.len());
            let mut word = &rest[..end];
            rest = &rest[end..];
            while cells_width(word) > width {
                if !row.is_empty() {
                    push(&mut out, &row);
                    row.clear();
                }
                let head = prefix_len(word, width);
                if head == 0 {
                    word = &word[1..];
                    continue;
                }
                push(&mut out, &word[..head]);
                word = &word[head..];
            }
            let Some(&(_, first)) = word.first() else {
                continue;
            };
            if row.is_empty() {
                row.extend_from_slice(word);
            } else if cells_width(&row) + 1 + cells_width(word) <= width {
                row.push((' ', gap.unwrap_or(first)));
                row.extend_from_slice(word);
            } else {
                push(&mut out, &row);
                row.clear();
                row.extend_from_slice(word);
            }
        }
        if !row.is_empty() || paragraph.is_empty() {
            push(&mut out, &row);
        }
    }
    out
}

/// A rule: `title`, then a dash track in `track`, then `right` when the whole of it fits. A title that does not
/// fit keeps its whole words.
#[must_use]
pub fn rule(
    width: usize,
    title: &Line<'_>,
    right: &Line<'_>,
    track: Style,
    colours: Colours,
) -> Line<'static> {
    let title = cut_styled(title, width, colours);
    let right = cells_of(right, colours);
    let (title_width, right_width) = (line_width(&title, colours), cells_width(&right));
    let mut spans = title.spans;
    if right_width == 0 || title_width + right_width + 2 > width {
        if title_width < width {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("─".repeat(width - title_width - 1), track));
        }
        return Line::from(spans);
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        "─".repeat(width - title_width - right_width - 2),
        track,
    ));
    spans.push(Span::raw(" "));
    spans.extend(line_of(&right).spans);
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    use test_case::test_case;

    use super::{
        Clause, Form, console_text, cut_marker, cut_styled, cut_words, display_width, fit_clauses,
        inert, is_console_glyph, line_width, plain, rule, truncate_styled, wrap_line, wrap_styled,
    };
    use crate::screen::Colours;

    const CONSOLE: Colours = Colours::Console;
    const RICH: Colours = Colours::Rich;

    #[test_case("idle until noon", 15, "idle until noon"; "fits")]
    #[test_case("so a freeze needs a manual reset", 22, "so a freeze needs a..."; "cut at a word")]
    #[test_case("heavy vector load", 13, "heavy..."; "never inside a word")]
    #[test_case("31 failures · 8 crashes", 17, "31 failures..."; "separator before the marker goes")]
    #[test_case("top", 2, ""; "one word that does not fit is omitted")]
    #[test_case("requester", 8, ""; "a long word that does not fit is omitted")]
    #[test_case("top", 0, ""; "no room")]
    #[test_case("██ ██ ██", 7, "██..."; "width counts cells, not bytes")]
    #[test_case("a … b", 5, "a..."; "marker glyph counts as its console form")]
    fn cut_words_keeps_whole_words_and_marks_the_cut(text: &str, width: usize, want: &str) {
        let got = cut_words(text, width, CONSOLE);
        assert_eq!(got, want, "cut_words({text:?}, {width})");
        assert!(
            display_width(&got, CONSOLE) <= width,
            "{got:?} is wider than {width}"
        );
    }

    #[test_case("idle until noon", 15, "idle until noon"; "fits")]
    #[test_case("heavy vector load", 13, "heavy vector…"; "the one-cell marker leaves room for another word")]
    #[test_case("heavy vector load", 12, "heavy…"; "never inside a word")]
    #[test_case("31 failures · 8 crashes", 14, "31 failures…"; "separator before the marker goes")]
    #[test_case("31 failures · 8 crashes", 17, "31 failures · 8…"; "the one-cell marker keeps another word")]
    #[test_case("a … b", 5, "a … b"; "marker glyph stays")]
    #[test_case("5 × 2m “q” — ok", 20, "5 × 2m “q” — ok"; "typography stays")]
    #[test_case("日本語 日本語", 13, "日本語 日本語"; "double-width words fit by cells")]
    #[test_case("日本語 日本語", 8, "日本語…"; "double-width words are cut by cells")]
    #[test_case("日本語 日本語", 6, ""; "a wide word with no room for the marker is omitted")]
    #[test_case("日本語 日本語", 7, "日本語…"; "exact wide word plus marker")]
    fn rich_cut_words_keep_unicode_whole_and_end_in_one_cell(text: &str, width: usize, want: &str) {
        let got = cut_words(text, width, RICH);
        assert_eq!(got, want, "cut_words({text:?}, {width})");
        assert!(
            display_width(&got, RICH) <= width,
            "{got:?} is wider than {width}"
        );
    }

    #[test]
    fn the_cut_marker_is_one_cell_only_on_rich() {
        assert_eq!(cut_marker(RICH), "…");
        assert_eq!(display_width(cut_marker(RICH), RICH), 1);
        for colours in [Colours::Full, Colours::Console, Colours::None] {
            assert_eq!(cut_marker(colours), "...", "{colours:?}");
        }
    }

    fn red() -> Style {
        Style::new().fg(Color::Red)
    }

    #[test_case(&Line::from("requester"), 2, ""; "below marker")]
    #[test_case(&Line::from("requester"), 3, ""; "marker only")]
    #[test_case(&Line::from("requester"), 8, ""; "one cell short")]
    #[test_case(&Line::from("requester"), 9, "requester"; "exact word")]
    #[test_case(&Line::from(Span::styled("requester", red())), 8, ""; "styled first word")]
    #[test_case(&Line::from(Span::styled("requester", red())), 9, "requester"; "styled exact word")]
    #[test_case(&Line::from("top requester"), 5, ""; "word fits but marker does not")]
    #[test_case(&Line::from("top requester"), 6, "top..."; "exact word plus marker")]
    #[test_case(&Line::from(Span::styled("top requester", red())), 6, "top..."; "styled word plus marker")]
    #[test_case(&Line::from("界"), 5, ""; "console escaped word")]
    #[test_case(&Line::from("界"), 6, r"\u754c"; "console escaped exact word")]
    fn cut_words_with_no_fitting_word_leaves_nothing(
        line: &Line<'static>,
        width: usize,
        want: &str,
    ) {
        let got = cut_styled(line, width, CONSOLE);
        assert_eq!(plain(&got, CONSOLE), want);
        if want.is_empty() {
            assert!(
                got.spans.is_empty(),
                "no fitting word leaves no marker or styling: {got:?}"
            );
        }
        assert!(
            line_width(&got, CONSOLE) <= width,
            "cut exceeds {width} cells"
        );
    }

    #[test]
    fn a_cut_keeps_the_style_of_the_words_it_kept() {
        let red = Style::new().fg(Color::Red);
        let cut = cut_styled(&Line::from(Span::styled("top requester", red)), 6, CONSOLE);
        assert!(cut.spans.iter().all(|span| span.style == red), "{cut:?}");
        let mixed = Line::from(vec![Span::styled("top ", red), Span::raw("requester")]);
        let cut = cut_styled(&mixed, 6, CONSOLE);
        assert_eq!(plain(&cut, CONSOLE), "top...");
        assert_eq!(cut.spans[0].style, red);
        let cut = cut_styled(&mixed, 5, RICH);
        assert_eq!(plain(&cut, RICH), "top…");
        assert_eq!(cut.spans[0].style, red);
    }

    #[test_case("SOLO LIMITS", 0, ""; "no room")]
    #[test_case("SOLO LIMITS", 2, ""; "two cells")]
    #[test_case("SOLO LIMITS", 8, "SOLO..."; "eight cells")]
    #[test_case("SOLO LIMITS", 10, "SOLO..."; "one cell short")]
    #[test_case("SOLO LIMITS", 11, "SOLO LIMITS"; "the whole title")]
    #[test_case("REQUESTERS", 8, ""; "a word wider than the room")]
    fn the_rule_keeps_whole_title_words(title: &str, width: usize, want: &str) {
        let line = rule(
            width,
            &Line::from(title),
            &Line::default(),
            Style::new(),
            CONSOLE,
        );
        assert_eq!(
            plain(&line, CONSOLE).trim_end_matches(['─', ' ']),
            want,
            "rule({width}, {title:?})"
        );
        assert!(
            line_width(&line, CONSOLE) <= width,
            "rule exceeds {width} cells: {}",
            plain(&line, CONSOLE)
        );
    }

    #[test]
    fn the_rule_puts_its_right_side_at_the_end_when_all_of_it_fits() {
        let right = Line::from("12:00");
        let track = Style::new().fg(Color::Magenta);
        let line = rule(20, &Line::from("NOW"), &right, track, CONSOLE);
        assert_eq!(
            plain(&line, CONSOLE),
            format!("NOW {} 12:00", "─".repeat(10))
        );
        assert_eq!(line_width(&line, CONSOLE), 20);
        let tight = rule(9, &Line::from("NOW"), &right, track, CONSOLE);
        assert_eq!(
            plain(&tight, CONSOLE),
            format!("NOW {}", "─".repeat(5)),
            "a right side that does not fit is left out"
        );
    }

    #[test]
    fn a_rich_rule_counts_double_width_titles_by_cells() {
        let line = rule(
            12,
            &Line::from("日本語"),
            &Line::from("終"),
            Style::new(),
            RICH,
        );
        assert_eq!(plain(&line, RICH), format!("日本語 {} 終", "─".repeat(2)));
        assert_eq!(line_width(&line, RICH), 12);
    }

    fn clauses() -> Vec<Clause> {
        vec![
            Clause::whole("", "returns to -37"),
            Clause::new(" · ", vec![Form::new("solo -40", 1), Form::empty()]),
            Clause::new(
                " · ",
                vec![
                    Form::new("held by C2", 3),
                    Form::new("C2", 2),
                    Form::empty(),
                ],
            ),
        ]
    }

    #[test_case(40, "returns to -37 · solo -40 · held by C2"; "everything fits")]
    #[test_case(37, "returns to -37 · solo -40 · C2"; "the longest form gives way")]
    #[test_case(29, "returns to -37 · solo -40"; "the last clause is left out")]
    #[test_case(24, "returns to -37"; "the second clause is left out")]
    #[test_case(14, "returns to -37"; "the first clause fits exactly")]
    #[test_case(13, "returns to..."; "the first clause is cut")]
    fn fit_clauses_leaves_out_lower_priority_clauses_before_cutting(width: usize, want: &str) {
        assert_eq!(
            plain(&fit_clauses(width, &clauses(), CONSOLE), CONSOLE),
            want,
            "width {width}"
        );
    }

    #[test_case(19, "日本語 · held by C2"; "everything fits")]
    #[test_case(18, "日本語 · C2"; "the longest form gives way")]
    #[test_case(8, "日本語"; "the optional clause is left out by cells")]
    #[test_case(5, ""; "a whole value that does not fit is cut")]
    fn rich_clauses_fit_by_display_cells(width: usize, want: &str) {
        let clauses = vec![
            Clause::whole("", "日本語"),
            Clause::new(
                " · ",
                vec![
                    Form::new("held by C2", 2),
                    Form::new("C2", 1),
                    Form::empty(),
                ],
            ),
        ];
        let line = fit_clauses(width, &clauses, RICH);
        assert_eq!(plain(&line, RICH), want, "width {width}");
        assert!(line_width(&line, RICH) <= width);
    }

    #[test]
    fn fit_clauses_without_clauses_is_empty() {
        assert!(fit_clauses(10, &[], CONSOLE).spans.is_empty());
    }

    #[test_case("5 × 2m", "5 x 2m"; "a multiplication sign")]
    #[test_case("parts…", "parts..."; "an ellipsis")]
    #[test_case("a — b – c", "a - b - c"; "dashes")]
    #[test_case("‘a’ “b”", "'a' \"b\""; "curly quotes")]
    #[test_case("░▄█ ·►", "░▄█ ·►"; "drawable block glyphs")]
    #[test_case("Tctl 95°C", "Tctl 95°C"; "a degree sign")]
    #[test_case("done ✓ 日", r"done \u2713 \u65e5"; "glyphs the font lacks")]
    #[test_case("plain ascii text", "plain ascii text"; "plain ascii")]
    fn console_text_replaces_what_the_font_cannot_draw(text: &str, want: &str) {
        assert_eq!(console_text(text), want, "console_text({text:?})");
    }

    #[test]
    fn inert_escapes_controls_visibly_and_is_idempotent() {
        let hostile =
            "日本語\x1b[2J\x1b]52;c;data\x07\r\n\u{9b}31m\u{9d}0;title\u{9c}\u{2028}\u{2029}";
        let escaped = r"\u65e5\u672c\u8a9e\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u009d0;title\u009c\u2028\u2029";
        for colours in [Colours::Full, Colours::Console, Colours::None] {
            assert_eq!(inert(hostile, colours), escaped, "{colours:?}");
            assert_eq!(inert(escaped, colours), escaped, "{colours:?}");
            assert!(inert(hostile, colours).chars().all(is_console_glyph));
        }
    }

    #[test]
    fn rich_inert_keeps_unicode_and_escapes_controls_visibly() {
        let hostile = "日本語 ✓ × … “q”\x1b[2J\x1b]52;c;data\x07\r\n\u{9b}31m\u{202e}\u{2028}";
        let escaped = r"日本語 ✓ × … “q”\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u202e\u2028";
        assert_eq!(inert(hostile, RICH), escaped);
        assert_eq!(inert(escaped, RICH), escaped, "idempotent");
        assert!(inert(hostile, RICH).chars().all(|c| !c.is_control()));
        assert!(matches!(
            inert("plain ascii", RICH),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test_case("⌨\u{fe0f}x", r"⌨\ufe0fx"; "emoji presentation selector")]
    #[test_case("a\u{fe0e}b", r"a\ufe0eb"; "text presentation selector")]
    #[test_case("a\u{e0100}b", r"a\ue0100b"; "supplementary variation selector")]
    #[test_case("👨\u{200d}👩\u{200d}👧", r"👨\u200d👩\u200d👧"; "zero-width joiner family")]
    #[test_case("👍\u{1f3fd}", r"👍\u1f3fd"; "skin-tone modifier")]
    #[test_case("\u{1f1ef}\u{1f1f5}", r"\u1f1ef\u1f1f5"; "regional-indicator flag")]
    #[test_case("🏴\u{e0067}\u{e007f}", r"🏴\ue0067\ue007f"; "emoji tag characters")]
    #[test_case("e\u{301}", "e\u{301}"; "ordinary combining marks stay")]
    fn rich_inert_escapes_grapheme_joining_characters(text: &str, want: &str) {
        assert_eq!(inert(text, RICH), want);
        assert_eq!(inert(want, RICH), want, "idempotent");
    }

    #[test]
    fn rich_inert_borrows_text_with_nothing_to_escape() {
        assert!(matches!(
            inert("日本語 e\u{301} ✓", RICH),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn rich_width_of_a_variation_selector_sequence_is_what_is_drawn() {
        // Every character left draws its own cells: the selector is visible text, so `x` is not covered.
        assert_eq!(display_width("⌨\u{fe0f}x", RICH), 1 + 6 + 1);
        assert_eq!(plain(&Line::from("⌨\u{fe0f}x"), RICH), r"⌨\ufe0fx");
    }

    #[test_case("ab ⌨\u{fe0f}x cd"; "variation selector")]
    #[test_case("ab 👨\u{200d}👩\u{200d}👧 cd"; "joiner family")]
    #[test_case("ab \u{1f1ef}\u{1f1f5} cd"; "flag")]
    #[test_case("ab 👍\u{1f3fd} cd"; "skin tone")]
    fn rich_cut_never_exceeds_the_room_with_joining_sequences(text: &str) {
        for room in 0..=display_width(text, RICH) + 1 {
            let got = cut_words(text, room, RICH);
            assert!(
                display_width(&got, RICH) <= room,
                "cut_words({text:?}, {room}) = {got:?} is wider than {room}"
            );
        }
    }

    #[test]
    fn rich_text_is_measured_as_the_terminal_draws_it() {
        assert_eq!(display_width("日本", RICH), 4);
        assert_eq!(display_width("日本", CONSOLE), 12, "console escapes it");
        assert_eq!(display_width("a×…b", RICH), 4);
        assert_eq!(display_width("a×…b", CONSOLE), 1 + 1 + 3 + 1);
        assert_eq!(display_width("\x1b", RICH), 4, "controls count as escaped");
        assert_eq!(
            display_width("e\u{301}", RICH),
            1,
            "a combining mark takes no cell"
        );
    }

    #[test]
    fn the_console_glyph_set_holds_what_the_frames_draw() {
        for glyph in "►▄░█▀▬·│─┌┐└┘→↑↓°".chars() {
            assert!(is_console_glyph(glyph), "{glyph:?}");
        }
        for glyph in "✓日…×—“".chars() {
            assert!(!is_console_glyph(glyph), "{glyph:?}");
        }
        assert_eq!(super::IBM437.chars().count(), 159);
    }

    #[test]
    fn undrawable_glyphs_are_measured_escaped() {
        let text = format!("journal error: {}", "界".repeat(60));
        let wrapped: String = wrap_styled(&text, 100, Style::new(), CONSOLE)
            .iter()
            .map(|line| plain(line, CONSOLE))
            .collect();
        assert_eq!(
            wrapped.matches(r"\u754c").count(),
            60,
            "wrapped escapes lost text: {wrapped:?}"
        );
        let cut = cut_words(&text, 30, CONSOLE);
        assert!(display_width(&cut, CONSOLE) <= 30, "trimmed escape {cut:?}");
        assert!(cut.starts_with("journal error"), "trimmed escape {cut:?}");
        assert_eq!(display_width("界", CONSOLE), 6);
        assert_eq!(display_width("\x1b", CONSOLE), 4);
    }

    #[test]
    fn wrap_breaks_between_words_and_inside_words_wider_than_the_width() {
        let lines = |text, width| {
            wrap_styled(text, width, Style::new(), CONSOLE)
                .iter()
                .map(|line| plain(line, CONSOLE))
                .collect::<Vec<_>>()
        };
        assert_eq!(lines("one two three", 7), ["one two", "three"]);
        assert_eq!(lines("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(lines("a\n\nb", 5), ["a", "", "b"]);
        assert_eq!(lines("", 5), [""]);
        assert!(lines("anything", 0).is_empty());
    }

    #[test]
    fn rich_wrap_keeps_unicode_and_breaks_by_display_cells() {
        let lines = |text, width| {
            wrap_styled(text, width, Style::new(), RICH)
                .iter()
                .map(|line| plain(line, RICH))
                .collect::<Vec<_>>()
        };
        assert_eq!(lines("日本 語 ok", 5), ["日本", "語 ok"]);
        assert_eq!(lines("日本語日本", 4), ["日本", "語日", "本"]);
        assert_eq!(
            lines("日a", 1),
            ["a"],
            "a character wider than the width is left out"
        );
        assert_eq!(lines("a\x1bb", 20), [r"a\x1bb"]);
    }

    #[test]
    fn a_wrapped_line_keeps_each_word_and_gap_in_its_own_style() {
        let red = Style::new().fg(Color::Red);
        let line = Line::from(vec![
            Span::raw("aaa "),
            Span::styled("bb cc", red),
            Span::raw(" dd"),
        ]);
        let styled = |line: &Line<'_>| {
            line.spans
                .iter()
                .map(|span| (span.content.to_string(), span.style == red))
                .collect::<Vec<_>>()
        };
        let rows: Vec<_> = wrap_line(&line, 6, CONSOLE).iter().map(styled).collect();
        assert_eq!(
            rows,
            [
                vec![("aaa ".to_owned(), false), ("bb".to_owned(), true)],
                vec![("cc".to_owned(), true), (" dd".to_owned(), false)],
            ]
        );
    }

    #[test]
    fn truncate_cuts_by_cells_inside_words() {
        let cut = truncate_styled(&Line::from("██ ██ ██"), 7, CONSOLE);
        assert_eq!(plain(&cut, CONSOLE), "██ ██ █");
    }

    #[test]
    fn rich_truncate_never_splits_a_double_width_character() {
        let cut = truncate_styled(&Line::from("日本語"), 5, RICH);
        assert_eq!(plain(&cut, RICH), "日本");
        assert_eq!(line_width(&cut, RICH), 4);
    }
}
