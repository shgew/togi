//! The note at the end of a core's row, and the top-requester note: lines that keep what they name at every
//! width.
//!
//! The note is chosen by the panels from the model ([`CoreNote`]); fitting it is
//! done here, so a value (an offset, a count, a core) is shortened to a bare
//! form before it is ever cut.

use ratatui::text::Line;
use togi_machine::TargetOffset;

use super::colours::Paint;
use super::fit::{Clause, Form, cut_words, fit_clauses, line_width, tint};
use crate::screen::Colours;

/// What a core's row says about the core, besides its gauge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreNote<'a> {
    /// A probed member: the offsets at which its group failed.
    GroupFailed(&'a [TargetOffset]),
    /// A parked core: the offset it returns to.
    ReturnsTo(TargetOffset),
    /// A search turn confirming a candidate solo limit.
    Confirming {
        /// The candidate offset.
        offset: TargetOffset,
        /// Light confirmations passed.
        light: u32,
        /// Heavy-vector confirmations passed.
        heavy: u32,
        /// Confirmations each load needs.
        needed: u32,
    },
    /// A core a combination holds, and its solo limit if it has one.
    HeldBy {
        /// The combination number.
        combination: u32,
        /// The solo limit.
        solo: Option<TargetOffset>,
    },
    /// A core that gave its depth back after a combination, and its solo limit if it has one.
    BackedOff {
        /// The combination number.
        after: u32,
        /// The solo limit.
        solo: Option<TargetOffset>,
    },
    /// A core whose solo limit is found.
    Found(TargetOffset),
    /// A core in search, with the offset it passed and the one it tries next.
    Searching {
        /// The deepest offset it passed.
        passed: Option<TargetOffset>,
        /// The offset it tries next.
        next: Option<TargetOffset>,
    },
    /// Why the core waits; untrusted text.
    Queued(&'a str),
}

/// Offsets as a space-separated list; `-` for none.
#[must_use]
pub fn offset_list(values: &[TargetOffset]) -> String {
    if values.is_empty() {
        return "-".to_owned();
    }
    values
        .iter()
        .map(|value| value.get().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

impl CoreNote<'_> {
    /// The note in `width` display cells. A clause the caller ranks lowest is shortened or left out first; the
    /// value the note is about is kept in a bare form down to its own width.
    #[must_use]
    pub fn line(&self, width: usize, colours: Colours) -> Line<'static> {
        let line = self.fitted(width, colours);
        #[cfg(test)]
        crate::corpus::line("note", "", &line);
        line
    }

    fn fitted(&self, width: usize, colours: Colours) -> Line<'static> {
        let sep = " · ";
        let one = |forms: Vec<Form>| Clause::new(sep, forms);
        let short = |paint: Paint, clauses: &[Clause]| {
            tint(fit_clauses(width, clauses, colours), paint.style(colours))
        };
        match *self {
            Self::GroupFailed(offsets) if !offsets.is_empty() => {
                let list = offset_list(offsets);
                short(
                    Paint::Text,
                    &[one(vec![
                        Form::new(format!("group failed at {list}"), 1),
                        Form::new(list, 0),
                    ])],
                )
            }
            Self::GroupFailed(_) => Line::default(),
            Self::ReturnsTo(back) => {
                let back = back.get();
                short(
                    Paint::Grey,
                    &[one(vec![
                        Form::new(format!("returns to {back}"), 1),
                        Form::new(format!("{back:4}"), 1),
                        Form::new(back.to_string(), 0),
                    ])],
                )
            }
            Self::Confirming {
                offset,
                light,
                heavy,
                needed,
            } => {
                let offset = offset.get();
                short(
                    Paint::Text,
                    &[one(vec![
                        Form::new(
                            format!("{offset}: light {light}/{needed}, heavy {heavy}/{needed}"),
                            1,
                        ),
                        Form::new(format!("L{light}/{needed} H{heavy}/{needed}"), 0),
                    ])],
                )
            }
            Self::HeldBy { combination, solo } => {
                let label = format!("C{combination}");
                let mut clauses = vec![one(vec![
                    Form::new(format!("held by {label}"), 2),
                    Form::new(label, 0),
                ])];
                clauses.extend(solo_clause(solo));
                short(Paint::Combination, &clauses)
            }
            Self::BackedOff { after, solo } => {
                let label = format!("C{after}");
                let mut clauses = vec![one(vec![
                    Form::new(format!("backed off after {label}"), 2),
                    Form::new(label, 0),
                ])];
                clauses.extend(solo_clause(solo));
                short(Paint::Grey, &clauses)
            }
            Self::Found(solo) => {
                let solo = solo.get();
                short(
                    Paint::Grey,
                    &[one(vec![
                        Form::new(format!("solo {solo}"), 1),
                        Form::new(solo.to_string(), 0),
                    ])],
                )
            }
            Self::Searching { passed, next } => {
                let mut clauses = Vec::new();
                if let Some(passed) = passed.map(TargetOffset::get) {
                    let mut forms = vec![
                        Form::new(format!("passed {passed}"), 4),
                        Form::new(format!("p {passed}"), 2),
                    ];
                    if next.is_some() {
                        forms.push(Form::empty());
                    }
                    clauses.push(one(forms));
                }
                if let Some(next) = next.map(TargetOffset::get) {
                    clauses.push(one(vec![
                        Form::new(format!("next {next}"), 3),
                        Form::new(format!("→ {next}"), 0),
                    ]));
                }
                short(Paint::Grey, &clauses)
            }
            Self::Queued(why) => {
                Line::styled(cut_words(why, width, colours), Paint::Grey.style(colours))
            }
        }
    }
}

fn solo_clause(solo: Option<TargetOffset>) -> Option<Clause> {
    solo.map(|solo| {
        let solo = solo.get();
        Clause::new(
            " · ",
            vec![Form::new(format!("solo {solo}"), 1), Form::empty()],
        )
    })
}

/// The top-requester note beside a core's own note: the word `top` in a form that fits, whole or left out.
/// `by_offset` says the offsets stand in for voltage requests; that note keeps its `offset` qualification in
/// every form, so where neither `top by offset` nor `offset top` fits it is left out rather than shown as `top`.
#[must_use]
pub fn top_note(
    note: &Line<'static>,
    by_offset: bool,
    width: usize,
    colours: Colours,
) -> Line<'static> {
    let (long, short) = if by_offset {
        ("top by offset", "offset top")
    } else {
        ("top requester", "top")
    };
    let word = if width >= "top requester".len() {
        long
    } else {
        short
    };
    if line_width(note, colours) == 0 {
        // Alone, the word is cut when it does not fit; an offset fallback must not be cut to a bare `top`.
        if width < word.len() {
            return Line::default();
        }
        return tint(
            fit_clauses(width, &[Clause::whole("", word)], colours),
            Paint::Text.style(colours),
        );
    }
    let mut forms = Vec::new();
    if by_offset {
        forms.push(Form::new(Line::styled(long, Paint::Text.style(colours)), 2));
        forms.push(Form::new(
            Line::styled(short, Paint::Text.style(colours)),
            1,
        ));
    } else {
        forms.push(Form::new(Line::styled(word, Paint::Text.style(colours)), 1));
    }
    forms.push(Form::empty());
    fit_clauses(
        width,
        &[
            Clause::whole("", note.clone()),
            Clause::new(Line::styled(" · ", Paint::Grey.style(colours)), forms),
        ],
        colours,
    )
}

#[cfg(test)]
mod tests {
    use ratatui::text::Line;
    use test_case::test_case;
    use togi_machine::TargetOffset;

    use super::{CoreNote, offset_list, top_note};
    use crate::frames::colours::Paint;
    use crate::frames::fit::{line_width, plain};
    use crate::screen::Colours;

    fn note(note: CoreNote<'_>, width: usize) -> String {
        plain(&note.line(width, Colours::Full), Colours::None)
    }

    fn t(offset: i64) -> TargetOffset {
        TargetOffset::new(offset).expect("a profile offset")
    }

    #[test_case(CoreNote::ReturnsTo(t(-37)), 20, "returns to -37"; "a return fits whole")]
    #[test_case(CoreNote::ReturnsTo(t(-37)), 14, "returns to -37"; "a return fits exactly")]
    #[test_case(CoreNote::ReturnsTo(t(-37)), 13, " -37"; "a return one cell short keeps the value")]
    #[test_case(CoreNote::ReturnsTo(t(-37)), 12, " -37"; "a return at 12 keeps the value")]
    #[test_case(CoreNote::ReturnsTo(t(-37)), 4, " -37"; "a return at 4 keeps the value")]
    #[test_case(CoreNote::ReturnsTo(t(-37)), 3, "-37"; "a return at 3 is the bare value")]
    #[test_case(CoreNote::Found(t(-38)), 13, "solo -38"; "a found limit fits whole")]
    #[test_case(CoreNote::Found(t(-38)), 7, "-38"; "a found limit shortens to the value")]
    #[test_case(CoreNote::BackedOff { after: 3, solo: Some(t(-38)) }, 17, "C3 · solo -38"; "a backoff fits whole")]
    #[test_case(CoreNote::BackedOff { after: 3, solo: Some(t(-38)) }, 6, "C3"; "a backoff shortens to its count")]
    fn a_note_line_keeps_its_value_at_the_width(core: CoreNote<'_>, width: usize, want: &str) {
        assert_eq!(note(core, width), want, "{core:?} in {width}");
    }

    #[test_case(CoreNote::GroupFailed(&[t(-30), t(-29), t(-27)]); "a group failure")]
    #[test_case(CoreNote::ReturnsTo(t(-37)); "a return")]
    #[test_case(CoreNote::Confirming { offset: t(-31), light: 2, heavy: 1, needed: 3 }; "a confirmation")]
    #[test_case(CoreNote::HeldBy { combination: 4, solo: Some(t(-31)) }; "a held core")]
    #[test_case(CoreNote::BackedOff { after: 12, solo: Some(t(-31)) }; "a backoff")]
    #[test_case(CoreNote::Found(t(-38)); "a found solo limit")]
    #[test_case(CoreNote::Searching { passed: Some(t(-20)), next: Some(t(-24)) }; "a search with a pass")]
    #[test_case(CoreNote::Searching { passed: None, next: Some(t(-24)) }; "a search without a pass")]
    #[test_case(CoreNote::Queued("waiting for the idle trial to end"); "a queued core")]
    fn a_note_shows_its_value_whole_or_nothing_at_every_width(core: CoreNote<'_>) {
        for width in 0..60 {
            let line = core.line(width, Colours::Full);
            let text = plain(&line, Colours::None);
            assert!(
                line_width(&line, Colours::None) <= width,
                "{core:?} in {width}: {text:?}"
            );
            for token in text
                .split(' ')
                .filter(|token| token.starts_with('-') && token.len() > 1)
            {
                assert!(
                    token[1..]
                        .trim_end_matches(['.', ':', ','])
                        .chars()
                        .all(|c| c.is_ascii_digit()),
                    "{core:?} in {width}: a value was cut: {text:?}"
                );
            }
            assert!(
                !text.starts_with(' ') || matches!(core, CoreNote::ReturnsTo(_)),
                "{core:?} in {width}: {text:?}"
            );
        }
    }

    #[test]
    fn notes_wear_their_tone() {
        let style = |core: CoreNote<'_>| core.line(40, Colours::Full).spans[0].style;
        assert_eq!(
            style(CoreNote::HeldBy {
                combination: 4,
                solo: None
            }),
            Paint::Combination.style(Colours::Full)
        );
        assert_eq!(
            style(CoreNote::Found(t(-1))),
            Paint::Grey.style(Colours::Full)
        );
        assert_eq!(
            style(CoreNote::Confirming {
                offset: t(-1),
                light: 0,
                heavy: 0,
                needed: 3
            }),
            Paint::Text.style(Colours::Full)
        );
        assert!(
            CoreNote::Found(t(-1))
                .line(40, Colours::None)
                .spans
                .iter()
                .all(|span| span.style == ratatui::style::Style::new())
        );
    }

    #[test]
    fn a_search_note_names_the_pass_and_the_next_offset_in_order_of_priority() {
        let both = CoreNote::Searching {
            passed: Some(t(-20)),
            next: Some(t(-24)),
        };
        assert_eq!(note(both, 40), "passed -20 · next -24");
        assert_eq!(note(both, 18), "p -20 · next -24");
        assert_eq!(note(both, 10), "→ -24");
        let only_next = CoreNote::Searching {
            passed: None,
            next: Some(t(-24)),
        };
        assert_eq!(note(only_next, 40), "next -24");
        assert_eq!(
            note(
                CoreNote::Searching {
                    passed: None,
                    next: None
                },
                40
            ),
            ""
        );
    }

    #[test]
    fn a_queued_note_is_escaped_untrusted_text() {
        assert_eq!(note(CoreNote::Queued("a\x1b[2Jb"), 40), r"a\x1b[2Jb");
        assert_eq!(
            note(CoreNote::Queued("waiting for the next trial"), 12),
            "waiting..."
        );
    }

    #[test]
    fn offsets_list_with_a_dash_for_none() {
        assert_eq!(offset_list(&[t(-30), t(-29)]), "-30 -29");
        assert_eq!(offset_list(&[]), "-");
    }

    #[test_case::test_case("", 2, ""; "empty too narrow")]
    #[test_case::test_case("", 3, "top"; "empty short")]
    #[test_case::test_case("", 12, "top"; "empty before full")]
    #[test_case::test_case("", 13, "top requester"; "empty full")]
    #[test_case::test_case("-11", 5, "-11"; "offset only")]
    #[test_case::test_case("-11", 9, "-11 · top"; "offset and top")]
    fn the_top_note_is_a_word_whole_or_nothing(text: &'static str, width: usize, want: &str) {
        assert_eq!(
            plain(
                &top_note(&Line::from(text), false, width, Colours::Full),
                Colours::None
            ),
            want,
            "top_note({text:?}, {width})"
        );
    }

    #[test_case::test_case("", 13, "top by offset"; "empty full")]
    #[test_case::test_case("", 12, "offset top"; "empty compact twelve")]
    #[test_case::test_case("", 10, "offset top"; "empty compact ten")]
    #[test_case::test_case("", 9, ""; "empty nine")]
    #[test_case::test_case("", 3, ""; "empty three")]
    #[test_case::test_case("-11", 20, "-11 · top by offset"; "offset full")]
    #[test_case::test_case("-11", 16, "-11 · offset top"; "offset compact")]
    #[test_case::test_case("-11", 15, "-11"; "offset fifteen")]
    #[test_case::test_case("-11", 12, "-11"; "offset twelve")]
    #[test_case::test_case("-11", 9, "-11"; "offset nine")]
    #[test_case::test_case("-11", 3, "-11"; "offset three")]
    fn an_offset_fallback_top_note_keeps_its_qualification_or_is_left_out(
        text: &'static str,
        width: usize,
        want: &str,
    ) {
        let got = plain(
            &top_note(&Line::from(text), true, width, Colours::Full),
            Colours::None,
        );
        assert_eq!(got, want, "top_note({text:?}, {width})");
        assert!(
            got != "top" && !got.ends_with(" top") || got.ends_with("offset top"),
            "an offset fallback reads as a measured top: {got:?}"
        );
    }
}
