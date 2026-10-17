//! Text fitting: every dashboard line is measured as the console will show it.
//!
//! Text from the journal is untrusted, so everything passes through [`inert`]
//! first: control characters become visible escapes
//! ([`togi_render::escape_text`]), then glyphs the console font cannot draw
//! become ASCII or a `\u` escape ([`console_text`]). Widths are display cells
//! of that escaped text, so a line never overflows because of what it
//! escapes to.
//!
//! A line that does not fit never cuts inside a value or a word: it first
//! shortens or leaves out whole clauses ([`fit_clauses`]), lowest priority
//! first, and text that must still be cut keeps the words that fit and ends
//! with [`CUT_MARKER`] ([`cut_words`]).

use std::borrow::Cow;
use std::fmt::Write as _;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use togi_render::escape_text;
use unicode_width::UnicodeWidthChar as _;

/// Ends text cut to fit; the console font has no `…`.
pub const CUT_MARKER: &str = "...";

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
pub fn console_text(text: &str) -> Cow<'_, str> {
    if text.chars().all(is_console_glyph) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            c if is_console_glyph(c) => out.push(c),
            '×' => out.push('x'),
            '…' => out.push_str(CUT_MARKER),
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

/// `text` as the screen may show it: untrusted control characters escaped, then what the console cannot draw.
/// Idempotent, so escaping twice changes nothing.
#[must_use]
pub fn inert(text: &str) -> Cow<'_, str> {
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
pub fn display_width(text: &str) -> usize {
    inert(text).chars().map(char_width).sum()
}

/// The display width of `line` once escaped for the screen.
#[must_use]
pub fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| display_width(&span.content))
        .sum()
}

/// `line` as plain text, as the screen shows it.
#[must_use]
pub fn plain(line: &Line<'_>) -> String {
    line.spans.iter().map(|span| inert(&span.content)).collect()
}

type Cell = (char, Style);

fn cells_of(line: &Line<'_>) -> Vec<Cell> {
    let mut out = Vec::new();
    for span in &line.spans {
        let style = line.style.patch(span.style);
        out.extend(inert(&span.content).chars().map(|c| (c, style)));
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
pub fn truncate_styled(line: &Line<'_>, width: usize) -> Line<'static> {
    let cells = cells_of(line);
    line_of(&cells[..prefix_len(&cells, width)])
}

/// `line` fitted to `width` display cells as the console will show it. Text that does not fit keeps the whole
/// words that fit and ends with [`CUT_MARKER`] in the style of the last kept word, or is left out when no word
/// fits.
#[must_use]
pub fn cut_styled(line: &Line<'_>, width: usize) -> Line<'static> {
    let mut cells = cells_of(line);
    if width == 0 {
        return Line::default();
    }
    if cells_width(&cells) <= width {
        return line_of(&cells);
    }
    let Some(room) = width.checked_sub(CUT_MARKER.len()) else {
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
    cells.extend(CUT_MARKER.chars().map(|c| (c, style)));
    line_of(&cells)
}

/// `text` fitted to `width` display cells; see [`cut_styled`].
#[must_use]
pub fn cut_words(text: &str, width: usize) -> String {
    plain(&cut_styled(&Line::from(text), width))
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
pub fn fit_clauses(width: usize, clauses: &[Clause]) -> Line<'static> {
    let mut at = vec![0; clauses.len()];
    loop {
        let mut line: Vec<Cell> = Vec::new();
        for (clause, &form) in clauses.iter().zip(&at) {
            let text = cells_of(&clause.forms[form].text);
            if text.is_empty() {
                continue;
            }
            if !line.is_empty() {
                line.extend(cells_of(&clause.sep));
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
            _ => return cut_styled(&line_of(&line), width),
        }
    }
}

/// `text` wrapped to `width` display cells in `style`; see [`wrap_line`].
#[must_use]
pub fn wrap_styled(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    wrap_line(&Line::from(Span::styled(text, style)), width)
}

/// `line` wrapped to `width` display cells, every word in its own style and every gap in the style of the space
/// it replaces. A newline starts a paragraph; a word wider than the width is broken by cells.
#[must_use]
pub(crate) fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
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
                paragraph.extend(inert(piece).chars().map(|c| (c, style)));
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
                let head = prefix_len(word, width).max(1);
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
pub fn rule(width: usize, title: &Line<'_>, right: &Line<'_>, track: Style) -> Line<'static> {
    let title = cut_styled(title, width);
    let right = cells_of(right);
    let (title_width, right_width) = (line_width(&title), cells_width(&right));
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
        CUT_MARKER, Clause, Form, console_text, cut_styled, cut_words, display_width, fit_clauses,
        inert, is_console_glyph, line_width, plain, rule, truncate_styled, wrap_line, wrap_styled,
    };

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
        let got = cut_words(text, width);
        assert_eq!(got, want, "cut_words({text:?}, {width})");
        assert!(
            display_width(&got) <= width,
            "{got:?} is wider than {width}"
        );
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
        let got = cut_styled(line, width);
        assert_eq!(plain(&got), want);
        if want.is_empty() {
            assert!(
                got.spans.is_empty(),
                "no fitting word leaves no marker or styling: {got:?}"
            );
        }
        assert!(line_width(&got) <= width, "cut exceeds {width} cells");
    }

    #[test]
    fn a_cut_keeps_the_style_of_the_words_it_kept() {
        let red = Style::new().fg(Color::Red);
        let cut = cut_styled(&Line::from(Span::styled("top requester", red)), 6);
        assert!(cut.spans.iter().all(|span| span.style == red), "{cut:?}");
        let mixed = Line::from(vec![Span::styled("top ", red), Span::raw("requester")]);
        let cut = cut_styled(&mixed, 6);
        assert_eq!(plain(&cut), "top...");
        assert_eq!(cut.spans[0].style, red);
    }

    #[test_case("SOLO LIMITS", 0, ""; "no room")]
    #[test_case("SOLO LIMITS", 2, ""; "two cells")]
    #[test_case("SOLO LIMITS", 8, "SOLO..."; "eight cells")]
    #[test_case("SOLO LIMITS", 10, "SOLO..."; "one cell short")]
    #[test_case("SOLO LIMITS", 11, "SOLO LIMITS"; "the whole title")]
    #[test_case("REQUESTERS", 8, ""; "a word wider than the room")]
    fn the_rule_keeps_whole_title_words(title: &str, width: usize, want: &str) {
        let line = rule(width, &Line::from(title), &Line::default(), Style::new());
        assert_eq!(
            plain(&line).trim_end_matches(['─', ' ']),
            want,
            "rule({width}, {title:?})"
        );
        assert!(
            line_width(&line) <= width,
            "rule exceeds {width} cells: {}",
            plain(&line)
        );
    }

    #[test]
    fn the_rule_puts_its_right_side_at_the_end_when_all_of_it_fits() {
        let right = Line::from("12:00");
        let track = Style::new().fg(Color::Magenta);
        let line = rule(20, &Line::from("NOW"), &right, track);
        assert_eq!(plain(&line), format!("NOW {} 12:00", "─".repeat(10)));
        assert_eq!(line_width(&line), 20);
        let tight = rule(9, &Line::from("NOW"), &right, track);
        assert_eq!(
            plain(&tight),
            format!("NOW {}", "─".repeat(5)),
            "a right side that does not fit is left out"
        );
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
            plain(&fit_clauses(width, &clauses())),
            want,
            "width {width}"
        );
    }

    #[test]
    fn fit_clauses_without_clauses_is_empty() {
        assert!(fit_clauses(10, &[]).spans.is_empty());
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
        assert_eq!(inert(hostile), escaped);
        assert_eq!(inert(escaped), escaped);
        assert!(inert(hostile).chars().all(is_console_glyph));
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
        let wrapped: String = wrap_styled(&text, 100, Style::new())
            .iter()
            .map(plain)
            .collect();
        assert_eq!(
            wrapped.matches(r"\u754c").count(),
            60,
            "wrapped escapes lost text: {wrapped:?}"
        );
        let cut = cut_words(&text, 30);
        assert!(display_width(&cut) <= 30, "trimmed escape {cut:?}");
        assert!(cut.starts_with("journal error"), "trimmed escape {cut:?}");
        assert_eq!(display_width("界"), 6);
        assert_eq!(display_width("\x1b"), 4);
    }

    #[test]
    fn wrap_breaks_between_words_and_inside_words_wider_than_the_width() {
        let lines = |text, width| {
            wrap_styled(text, width, Style::new())
                .iter()
                .map(plain)
                .collect::<Vec<_>>()
        };
        assert_eq!(lines("one two three", 7), ["one two", "three"]);
        assert_eq!(lines("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(lines("a\n\nb", 5), ["a", "", "b"]);
        assert_eq!(lines("", 5), [""]);
        assert!(lines("anything", 0).is_empty());
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
        let rows: Vec<_> = wrap_line(&line, 6).iter().map(styled).collect();
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
        let cut = truncate_styled(&Line::from("██ ██ ██"), 7);
        assert_eq!(plain(&cut), "██ ██ █");
        assert_eq!(CUT_MARKER, "...");
    }
}
