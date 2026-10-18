//! The key hints on the last row of the frame.

use ratatui::buffer::Buffer;
use ratatui::text::{Line, Span};
use togi_render::escape_text;

use super::colours::Paint;
use super::draw::put;
use super::fit::{Clause, Form, fit_clauses};
use super::layout::Layout;
use crate::screen::{Colours, Quit, Screen, View};

/// The footer's keys in `view`, in `width` display cells, followed by `after` (the held journal's arrival
/// count, then the skipped report).
///
/// Labels shorten before a key is left out. All four keys stay down to 20 cells, with or without `after`; below
/// that every key but the exit key may go, and `q` is shown whole whenever one cell fits. Active and
/// current-view styling stays the same in the shortened forms. With [`Quit::Ignored`] the exit key is not a key
/// at all: no hint names `q`, and the other three go first as the line narrows.
#[must_use]
pub fn key_hints(
    view: View,
    width: usize,
    after: &[Clause],
    colours: Colours,
    quit: Quit,
) -> Line<'static> {
    struct Hint {
        name: &'static str,
        label: &'static str,
        active: bool,
        current: bool,
    }
    let keys = [
        Hint {
            name: "?",
            label: "help",
            active: true,
            current: view == View::Help,
        },
        Hint {
            name: "l",
            label: "log",
            active: true,
            current: view == View::Log,
        },
        Hint {
            name: "esc",
            label: "back",
            active: view != View::Main,
            current: false,
        },
        Hint {
            name: "q",
            label: "exit dashboard",
            active: true,
            current: false,
        },
    ];
    let mut clauses = Vec::with_capacity(keys.len() + after.len());
    for key in keys
        .into_iter()
        .take(if quit == Quit::Keys { 4 } else { 3 })
    {
        let (chip, label) = if key.current {
            (Paint::Current, Paint::Grey)
        } else if key.active {
            (Paint::Chip, Paint::Grey)
        } else {
            (Paint::Track, Paint::Track)
        };
        let pair = |key_text: String, label_text: &'static str| {
            Line::from(vec![
                chip.span(colours, key_text),
                Span::raw(" "),
                label.span(colours, label_text),
            ])
        };
        let mut forms = vec![Form::new(pair(format!(" {} ", key.name), key.label), 10)];
        if key.name == "q" {
            forms.push(Form::new(pair(" q ".to_owned(), "exit"), 10));
        }
        forms.push(Form::new(chip.span(colours, key.name), 1));
        if key.name != "q" {
            forms.push(Form::empty());
        }
        clauses.push(Clause::new("   ", forms));
    }
    clauses.extend_from_slice(after);
    fit_clauses(width, &clauses)
}

/// The footer clause for what the lenient read of the journal skipped or ignored, last in the footer's `after`.
///
/// It reads the report whole, then `skipped journal content: togi events` pointing at the command that prints
/// it, then `skipped: togi events`, `skipped` and an amber `!`. Every key label shortens before the warning
/// does, and the arrival count is left out before the warning shortens past `skipped`. The `!` outlasts `?`,
/// `l` and `esc`, so all four keys and the warning share 20 cells, and below 19 cells the three go first and
/// `q` alone fills 1 to 4.
#[must_use]
pub(super) fn skipped_clause(skipped: &str, colours: Colours) -> Clause {
    let amber =
        |text: &str| Line::styled(escape_text(text).to_string(), Paint::Hunt.style(colours));
    Clause::new(
        "   ",
        vec![
            Form::new(amber(skipped), 20),
            Form::new(amber("skipped journal content: togi events"), 3),
            Form::new(amber("skipped: togi events"), 3),
            Form::new(amber("skipped"), 1),
            Form::new(amber("!"), 0),
            Form::empty(),
        ],
    )
}

/// Draws the footer row: the key hints when the reader has keys, with `after` following them; without keys,
/// `after` alone.
pub fn footer(screen: &Screen, after: &[Clause], buf: &mut Buffer) {
    let layout = Layout::new(screen);
    let width = usize::from(layout.footer.width);
    let line = if screen.keys {
        key_hints(screen.view, width, after, screen.colours, screen.quit)
    } else if after.is_empty() {
        return;
    } else {
        fit_clauses(width, after)
    };
    put(buf, layout.footer, 0, 0, &line);
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    use super::{footer, key_hints, skipped_clause};
    use crate::frames::colours::Paint;
    use crate::frames::fit::{Clause, Form, display_width, plain};
    use crate::frames::layout::Layout;
    use crate::frames::testing::{assert_frame_bounds, draw_checked, screen};
    use crate::screen::{Colours, Drawn, Hold, Quit, Screen, View};

    const VIEWS: [View; 3] = [View::Main, View::Help, View::Log];

    #[test]
    fn the_skipped_report_follows_the_keys_whole_and_then_as_a_pointer_that_keeps_them() {
        let report = "skipped 1 event of a kind this build does not know (future.thing at line 3)";
        let after = [skipped_clause(report, Colours::None)];
        let wide = plain(&key_hints(
            View::Main,
            200,
            &after,
            Colours::None,
            Quit::Keys,
        ));
        assert!(wide.contains(report), "{wide}");
        let narrow = plain(&key_hints(
            View::Main,
            100,
            &after,
            Colours::None,
            Quit::Keys,
        ));
        assert!(narrow.contains("togi events"), "{narrow}");
        let fields: Vec<&str> = narrow.split_whitespace().collect();
        for key in ["?", "l", "esc", "q"] {
            assert!(fields.contains(&key), "{narrow}");
        }
    }

    const REPORT: &str =
        "skipped 1 event of a kind this build does not know (future.thing at line 3)";

    #[test]
    fn the_skipped_warning_never_costs_a_key_down_to_twenty_cells() {
        let after = [skipped_clause(REPORT, Colours::None)];
        for view in VIEWS {
            for width in 20..=120 {
                let text = plain(&key_hints(view, width, &after, Colours::None, Quit::Keys));
                assert!(display_width(&text) <= width, "{view:?} {width}: {text:?}");
                let fields: Vec<&str> = text.split_whitespace().collect();
                for key in ["?", "l", "esc", "q"] {
                    assert!(
                        fields.contains(&key),
                        "{view:?} {width}: lost {key:?}: {text:?}"
                    );
                }
                assert!(
                    text.contains('!') || text.contains("skipped"),
                    "{view:?} {width}: the warning is gone: {text:?}"
                );
            }
        }
    }

    #[test]
    fn the_skipped_warning_shortens_by_steps_at_normal_narrow_widths() {
        let after = [skipped_clause(REPORT, Colours::None)];
        let text = |width| {
            plain(&key_hints(
                View::Help,
                width,
                &after,
                Colours::None,
                Quit::Keys,
            ))
        };
        assert!(text(20).ends_with("   !"), "{}", text(20));
        assert!(text(30).ends_with("   skipped"), "{}", text(30));
        assert!(
            text(40).ends_with("   skipped: togi events"),
            "{}",
            text(40)
        );
        assert!(
            text(50).ends_with("   skipped: togi events"),
            "{}",
            text(50)
        );
        assert!(text(200).ends_with(REPORT), "{}", text(200));
    }

    #[test]
    fn the_skipped_warning_leaves_the_close_key_whole_at_any_width() {
        let after = [skipped_clause(REPORT, Colours::None)];
        for view in VIEWS {
            for width in 1..20 {
                let text = plain(&key_hints(view, width, &after, Colours::None, Quit::Keys));
                assert!(display_width(&text) <= width, "{view:?} {width}: {text:?}");
                assert!(
                    text.split_whitespace().any(|key| key == "q"),
                    "{view:?} {width}: {text:?}"
                );
                if width >= 5 {
                    assert!(text.contains('!'), "{view:?} {width}: {text:?}");
                }
            }
            assert_eq!(
                plain(&key_hints(view, 0, &after, Colours::None, Quit::Keys)),
                ""
            );
        }
    }

    #[test]
    fn the_skipped_warning_keeps_its_amber_and_the_keys_their_style() {
        let after = [skipped_clause(REPORT, Colours::Full)];
        for view in VIEWS {
            let reference = key_hints(view, 80, &[], Colours::Full, Quit::Keys);
            for width in [20, 30, 40, 50] {
                let line = key_hints(view, width, &after, Colours::Full, Quit::Keys);
                for key in ["?", "l", "esc", "q"] {
                    let style = |line: &ratatui::text::Line<'_>| {
                        line.spans
                            .iter()
                            .find(|span| span.content.trim() == key)
                            .map(|span| span.style)
                    };
                    assert_eq!(style(&line), style(&reference), "{view:?} {width}: {key:?}");
                }
                let last = line.spans.last().expect("a footer");
                assert_eq!(
                    last.style,
                    Paint::Hunt.style(Colours::Full),
                    "{view:?} {width}: the warning lost its amber"
                );
            }
        }
    }

    fn draw_footer(view: View, width: u16, height: u16, colours: Colours) -> (Buffer, Rect) {
        let sc = screen(view, width, height, colours, true);
        let (buf, _) = draw_checked(&sc, |sc, buf| {
            footer(sc, &[], buf);
            Drawn {
                scroll: 0,
                hold: Hold::Never,
            }
        });
        let layout = Layout::new(&sc);
        (buf, layout.footer)
    }

    fn footer_text(buf: &Buffer, rect: Rect) -> String {
        (rect.x..rect.right())
            .map(|x| buf[(x, rect.y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn the_footer_keeps_all_four_keys_at_twenty_cells_and_wider() {
        for view in VIEWS {
            let (reference, wide_rect) = draw_footer(view, 80, 12, Colours::Full);
            let wide_text = footer_text(&reference, wide_rect);
            for width in [20, 30, 40, 50] {
                let (buf, rect) = draw_footer(view, width, 12, Colours::Full);
                let text = footer_text(&buf, rect);
                let fields: Vec<&str> = text.split_whitespace().collect();
                for key in ["?", "l", "esc", "q"] {
                    assert!(
                        fields.contains(&key),
                        "{view:?} width {width}: footer lost whole key {key:?}: {text:?}"
                    );
                    let at = |text: &str, rect: Rect| {
                        let chars: Vec<char> = text.chars().collect();
                        let key: Vec<char> = key.chars().collect();
                        (0..chars.len())
                            .find(|&i| {
                                chars[i..].starts_with(&key)
                                    && (i == 0 || chars[i - 1] == ' ')
                                    && chars.get(i + key.len()).is_none_or(|c| *c == ' ')
                            })
                            .map(|i| (rect.x + u16::try_from(i).unwrap(), rect.y))
                    };
                    let (Some(wide_at), Some(at)) = (at(&wide_text, wide_rect), at(&text, rect))
                    else {
                        panic!("{view:?} width {width}: key {key:?} not found");
                    };
                    let (want, got) = (&reference[wide_at], &buf[at]);
                    assert_eq!(
                        (got.fg, got.bg, got.modifier),
                        (want.fg, want.bg, want.modifier),
                        "{view:?} width {width}: key {key:?} lost its active/current style"
                    );
                }
                assert!(
                    !text.contains("..."),
                    "{view:?} width {width}: key hints must shorten labels, not cut keys: {text:?}"
                );
            }
        }
    }

    #[test]
    fn the_footer_keeps_the_close_key_first() {
        for view in VIEWS {
            for width in [4, 5, 6, 10] {
                let (buf, rect) = draw_footer(view, width, 12, Colours::Full);
                let text = footer_text(&buf, rect);
                let keys: Vec<&str> = text.split_whitespace().collect();
                assert!(
                    keys.contains(&"q"),
                    "{view:?} width {width}: closing key must survive first: {text:?}"
                );
                for key in keys {
                    assert!(
                        ["?", "l", "esc", "q"].contains(&key),
                        "{view:?} width {width}: tiny footer cut a key or label: {text:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn q_is_the_last_key_left_out_and_shown_whole_when_one_cell_fits() {
        for view in VIEWS {
            for width in 1..20 {
                let line = key_hints(view, width, &[], Colours::None, Quit::Keys);
                let text = plain(&line);
                assert!(display_width(&text) <= width, "{view:?} {width}: {text:?}");
                assert!(
                    text.split_whitespace().any(|key| key == "q"),
                    "{view:?} {width}: {text:?}"
                );
            }
            assert_eq!(
                plain(&key_hints(view, 0, &[], Colours::None, Quit::Keys)),
                ""
            );
        }
    }

    #[test]
    fn the_full_footer_names_every_key_and_marks_the_view_on_screen() {
        let line = key_hints(View::Help, 80, &[], Colours::Full, Quit::Keys);
        assert_eq!(
            plain(&line),
            " ?  help    l  log    esc  back    q  exit dashboard"
        );
        let style_of = |text: &str| {
            line.spans
                .iter()
                .find(|span| span.content == text)
                .map(|span| span.style)
        };
        assert_eq!(style_of(" ? "), Some(Paint::Current.style(Colours::Full)));
        assert_eq!(style_of(" l "), Some(Paint::Chip.style(Colours::Full)));
        assert_eq!(style_of(" q "), Some(Paint::Chip.style(Colours::Full)));
        let main = key_hints(View::Main, 80, &[], Colours::Full, Quit::Keys);
        let esc = main
            .spans
            .iter()
            .find(|span| span.content == " esc ")
            .map(|span| span.style);
        assert_eq!(
            esc,
            Some(Paint::Track.style(Colours::Full)),
            "esc has nothing to go back to on the main view"
        );
    }

    #[test]
    fn without_colour_the_footer_carries_no_style() {
        for view in VIEWS {
            let line = key_hints(view, 80, &[], Colours::None, Quit::Keys);
            assert!(line.spans.iter().all(|span| span.style == Style::new()));
        }
    }

    #[test]
    fn the_arrival_count_follows_the_keys_and_shortens_after_their_labels() {
        let arrival = vec![Clause::new(
            "   ",
            vec![
                Form::new("3 new events · End: latest", 2),
                Form::new("3 new events", 2),
                Form::empty(),
            ],
        )];
        let text = |width| {
            plain(&key_hints(
                View::Log,
                width,
                &arrival,
                Colours::None,
                Quit::Keys,
            ))
        };
        assert_eq!(
            text(80),
            " ?  help    l  log    esc  back    q  exit   3 new events · End: latest"
        );
        for width in 20..=80 {
            let text = text(width);
            let fields: Vec<&str> = text.split_whitespace().collect();
            for key in ["?", "l", "esc", "q"] {
                assert!(
                    fields.contains(&key),
                    "width {width}: footer lost key {key:?}: {text:?}"
                );
            }
            if text.contains("new") {
                assert!(
                    text.ends_with("3 new events · End: latest") || text.ends_with("3 new events"),
                    "width {width}: the arrival count is cut: {text:?}"
                );
            }
        }
        assert!(!text(20).contains("new"), "{}", text(20));
    }

    #[test]
    fn no_keys_no_footer() {
        let sc = screen(View::Help, 80, 24, Colours::Full, false);
        let (buf, _) = draw_checked(&sc, |sc, buf| {
            footer(sc, &[], buf);
            Drawn {
                scroll: 0,
                hold: Hold::Never,
            }
        });
        assert!(buf.content().iter().all(|cell| cell.symbol() == " "));
    }

    #[test]
    fn the_footer_stays_in_bounds_at_every_size() {
        for view in VIEWS {
            for width in 0..=60 {
                for height in 0..=14 {
                    for colours in [Colours::Full, Colours::Console, Colours::None] {
                        let sc = screen(view, width, height, colours, true);
                        let (buf, _) = draw_checked(&sc, |sc, buf| {
                            footer(sc, &[], buf);
                            Drawn {
                                scroll: 0,
                                hold: Hold::Never,
                            }
                        });
                        assert_frame_bounds(&buf, &sc);
                    }
                }
            }
        }
    }

    #[test]
    fn the_tuning_boot_footer_never_names_a_quit_key() {
        let skipped = [skipped_clause(REPORT, Colours::None)];
        for view in VIEWS {
            for width in 0..=120 {
                for after in [&[][..], &skipped[..]] {
                    let text = plain(&key_hints(view, width, after, Colours::None, Quit::Ignored));
                    assert!(display_width(&text) <= width, "{view:?} {width}: {text:?}");
                    for word in text.split_whitespace() {
                        assert!(
                            !["q", "exit", "dashboard", "close"].contains(&word),
                            "{view:?} {width}: the footer names a quit key: {text:?}"
                        );
                    }
                }
            }
            assert_eq!(
                plain(&key_hints(view, 80, &[], Colours::None, Quit::Ignored)),
                " ?  help    l  log    esc  back"
            );
        }
    }

    #[test]
    fn the_drawn_tuning_boot_footer_follows_the_screen_policy() {
        for view in VIEWS {
            let sc = Screen {
                quit: Quit::Ignored,
                ..screen(view, 80, 12, Colours::None, true)
            };
            let (buf, _) = draw_checked(&sc, |sc, buf| {
                footer(sc, &[], buf);
                Drawn {
                    scroll: 0,
                    hold: Hold::Never,
                }
            });
            let text = footer_text(&buf, Layout::new(&sc).footer);
            assert_eq!(text, " ?  help    l  log    esc  back", "{view:?}");
        }
    }
}
