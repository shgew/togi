//! The note at the end of a core's row: a line that keeps what it names at every width.
//!
//! The note is chosen by the panels from the model ([`CoreNote`]); fitting it is
//! done here, so a value (an offset, a count, a core) is shortened to a bare
//! form before it is ever cut.

use ratatui::text::Line;
use togi_machine::TargetOffset;

use super::colours::Paint;
use super::fit::{Clause, Form, cut_words, fit_clauses, tint};
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
            tint(fit_clauses(width, clauses), paint.style(colours))
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
            Self::Queued(why) => Line::styled(cut_words(why, width), Paint::Grey.style(colours)),
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

#[cfg(test)]
mod tests {
    use togi_machine::TargetOffset;

    use super::{CoreNote, offset_list};
    use crate::frames::colours::Paint;
    use crate::frames::fit::{line_width, plain};
    use crate::screen::Colours;

    fn note(note: CoreNote<'_>, width: usize) -> String {
        plain(&note.line(width, Colours::Full))
    }

    fn t(offset: i64) -> TargetOffset {
        TargetOffset::new(offset).expect("a profile offset")
    }

    #[test]
    fn a_note_line_keeps_its_value_at_every_width() {
        let returns = CoreNote::ReturnsTo(t(-37));
        let found = CoreNote::Found(t(-38));
        let backed = CoreNote::BackedOff {
            after: 3,
            solo: Some(t(-38)),
        };
        for (core, width, want) in [
            (returns, 20, "returns to -37"),
            (returns, 14, "returns to -37"),
            (returns, 13, " -37"),
            (returns, 12, " -37"),
            (returns, 4, " -37"),
            (returns, 3, "-37"),
            (found, 13, "solo -38"),
            (found, 7, "-38"),
            (backed, 17, "C3 · solo -38"),
            (backed, 6, "C3"),
        ] {
            assert_eq!(note(core, width), want, "{core:?} in {width}");
        }
    }

    #[test]
    fn every_note_shows_its_value_whole_or_nothing_at_every_width() {
        let offsets = [t(-30), t(-29), t(-27)];
        for core in [
            CoreNote::GroupFailed(&offsets),
            CoreNote::ReturnsTo(t(-37)),
            CoreNote::Confirming {
                offset: t(-31),
                light: 2,
                heavy: 1,
                needed: 3,
            },
            CoreNote::HeldBy {
                combination: 4,
                solo: Some(t(-31)),
            },
            CoreNote::BackedOff {
                after: 12,
                solo: Some(t(-31)),
            },
            CoreNote::Found(t(-38)),
            CoreNote::Searching {
                passed: Some(t(-20)),
                next: Some(t(-24)),
            },
            CoreNote::Searching {
                passed: None,
                next: Some(t(-24)),
            },
            CoreNote::Queued("waiting for the idle trial to end"),
        ] {
            for width in 0..60 {
                let line = core.line(width, Colours::Full);
                let text = plain(&line);
                assert!(line_width(&line) <= width, "{core:?} in {width}: {text:?}");
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
}
