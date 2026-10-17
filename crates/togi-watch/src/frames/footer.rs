//! The last row of the frame: the keys of the nine pages for a reader with keys, the page's identity and the page
//! names for a print.

use ratatui::buffer::Buffer;
use ratatui::text::{Line, Span};
use togi_render::escape_text;

use super::colours::Paint;
use super::draw::put;
use super::fit::{Clause, Form, fit_clauses};
use super::layout::Layout;
use crate::screen::{Colours, Quit, Screen, View};

/// The gap between two clauses of the footer.
const GAP: &str = "  ";

/// The page's place in the cycle and its name: `2/9 Cores`.
fn identity(view: View) -> String {
    format!("{}/{} {}", view.number(), View::ALL.len(), view.name())
}

/// The footer's keys on `view`, in `width` display cells, followed by `after` (the held journal's arrival count,
/// then the skipped report).
///
/// Every page shows `←/→ pages`, its own place `2/9 Cores`, `↑/↓ scroll`, `PgUp/PgDn`, `Esc Main` and `? help`,
/// then `q exit dashboard` under [`Quit::Keys`]. The line narrows by whole clauses: the page keys leave first, then
/// the scroll keys, then every label shortens before a key is left out, so the four keys `←/→`, `Esc`, `?` and `q`
/// outlast the page's place, which shortens to `2/9` and then goes, and `q` is shown whole whenever one cell fits.
/// Help teaches the keys that left. Active and current-page styling stays the same in the shortened forms. With
/// [`Quit::Ignored`] the exit key is not a key at all: no hint names `q`.
#[must_use]
pub fn key_hints(
    view: View,
    width: usize,
    after: &[Clause],
    colours: Colours,
    quit: Quit,
) -> Line<'static> {
    let pair = |chip: Paint, label: Paint, key: &str, text: &str| {
        let mut spans = vec![chip.span(colours, format!(" {key} "))];
        if !text.is_empty() {
            spans.push(Span::raw(" "));
            spans.push(label.span(colours, text.to_owned()));
        }
        Line::from(spans)
    };
    let key = |key: &'static str, text: &'static str, active: bool, current: bool| {
        let (chip, label) = if current {
            (Paint::Current, Paint::Grey)
        } else if active {
            (Paint::Chip, Paint::Grey)
        } else {
            (Paint::Track, Paint::Track)
        };
        Clause::new(
            GAP,
            vec![
                Form::new(pair(chip, label, key, text), 10),
                Form::new(chip.span(colours, key), 1),
                Form::empty(),
            ],
        )
    };
    let leaves = |keys: &'static str, text: &'static str, rank: u8| {
        Clause::new(
            GAP,
            vec![
                Form::new(pair(Paint::Chip, Paint::Grey, keys, text), rank),
                Form::empty(),
            ],
        )
    };
    let place = Clause::new(
        GAP,
        vec![
            Form::new(Paint::White.span(colours, identity(view)), 10),
            Form::new(
                Paint::White.span(colours, format!("{}/{}", view.number(), View::ALL.len())),
                3,
            ),
            Form::empty(),
        ],
    );
    let mut clauses = vec![
        key("←/→", "pages", true, false),
        place,
        leaves("↑/↓", "scroll", 11),
        leaves("PgUp/PgDn", "", 12),
        key("Esc", "Main", view != View::Main, false),
        key("?", "help", true, view == View::Help),
    ];
    if quit == Quit::Keys {
        let chip = Paint::Chip;
        clauses.push(Clause::new(
            GAP,
            vec![
                Form::new(pair(chip, Paint::Grey, "q", "exit dashboard"), 10),
                Form::new(pair(chip, Paint::Grey, "q", "exit"), 10),
                Form::new(chip.span(colours, "q"), 1),
            ],
        ));
    }
    clauses.extend_from_slice(after);
    fit_clauses(width, &clauses)
}

/// What a print shows instead of keys it cannot be pressed with: the page's place and name, then the names of all
/// nine pages with this one in brackets, followed by `after`. The names go first as the line narrows, then the
/// name of the page and its number.
fn page_names(view: View, width: usize, after: &[Clause], colours: Colours) -> Line<'static> {
    let mut names = Vec::new();
    for (at, page) in View::ALL.into_iter().enumerate() {
        if at > 0 {
            names.push(Paint::Grey.span(colours, " | "));
        }
        names.push(if page == view {
            Paint::Current.span(colours, format!("[{}]", page.name()))
        } else {
            Paint::Grey.span(colours, page.name())
        });
    }
    let mut clauses = vec![
        Clause::new(
            GAP,
            vec![
                Form::new(Paint::White.span(colours, identity(view)), 1),
                Form::new(
                    Paint::White.span(colours, format!("{}/{}", view.number(), View::ALL.len())),
                    1,
                ),
                Form::empty(),
            ],
        ),
        Clause::new(GAP, vec![Form::new(Line::from(names), 10), Form::empty()]),
    ];
    clauses.extend_from_slice(after);
    fit_clauses(width, &clauses)
}

/// The footer clause for what the lenient read of the journal skipped or ignored, last in the footer's `after`.
///
/// It reads the report whole, then `skipped journal content: togi events` pointing at the command that prints
/// it, then `skipped: togi events`, `skipped` and an amber `!`. Every key label shortens before the warning
/// does, and the arrival count is left out before the warning shortens past `skipped`. The `!` outlasts the keys
/// `←/→`, `Esc` and `?`, so all four keys and the warning share 17 cells, and below 17 cells those three go first
/// and `q` alone fills 1 to 4.
#[must_use]
pub(super) fn skipped_clause(skipped: &str, colours: Colours) -> Clause {
    let amber =
        |text: &str| Line::styled(escape_text(text).to_string(), Paint::Hunt.style(colours));
    Clause::new(
        GAP,
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

/// Draws the footer row: the key hints when the reader has keys, with `after` following them; for a print, the
/// page's place and the page names, with `after`.
pub fn footer(screen: &Screen, after: &[Clause], buf: &mut Buffer) {
    let layout = Layout::new(screen);
    let width = usize::from(layout.footer.width);
    let line = if screen.keys {
        key_hints(screen.view, width, after, screen.colours, screen.quit)
    } else {
        page_names(screen.view, width, after, screen.colours)
    };
    put(buf, layout.footer, 0, 0, &line);
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    use super::{footer, identity, key_hints, page_names, skipped_clause};
    use crate::frames::colours::Paint;
    use crate::frames::fit::{Clause, Form, display_width, plain};
    use crate::frames::layout::Layout;
    use crate::frames::testing::{
        assert_frame_bounds, assert_glyphs_drawable, draw_checked, screen,
    };
    use crate::screen::{Colours, Drawn, Hold, Quit, Screen, View};

    const REPORT: &str =
        "skipped 1 event of a kind this build does not know (future.thing at line 3)";

    /// The four keys that outlast every label.
    const KEYS: [&str; 4] = ["←/→", "Esc", "?", "q"];

    fn fields(text: &str) -> Vec<&str> {
        text.split_whitespace().collect()
    }

    fn hints(view: View, width: usize, after: &[Clause], quit: Quit) -> String {
        plain(&key_hints(view, width, after, Colours::None, quit))
    }

    /// The footer on `view` at full width, built from its clauses.
    fn full(view: View, quit: Quit) -> String {
        let mut clauses = vec![
            " ←/→  pages".to_owned(),
            identity(view),
            " ↑/↓  scroll".to_owned(),
            " PgUp/PgDn ".to_owned(),
            " Esc  Main".to_owned(),
            " ?  help".to_owned(),
        ];
        if quit == Quit::Keys {
            clauses.push(" q  exit dashboard".to_owned());
        }
        clauses.join("  ")
    }

    #[test]
    fn the_full_footer_names_every_key_and_the_pages_place_on_every_page() {
        for view in View::ALL {
            assert_eq!(hints(view, 300, &[], Quit::Keys), full(view, Quit::Keys));
            assert_eq!(
                hints(view, 300, &[], Quit::Ignored),
                full(view, Quit::Ignored)
            );
        }
        assert!(full(View::Main, Quit::Keys).contains("1/9 Main"));
        assert!(full(View::Cores, Quit::Keys).contains("2/9 Cores"));
        assert!(full(View::Log, Quit::Keys).contains("8/9 Journal"));
    }

    #[test]
    fn the_line_narrows_by_whole_clauses_in_the_order_the_keys_matter() {
        for view in View::ALL {
            let wide = full(view, Quit::Keys);
            let wide_width = display_width(&wide);
            let pages = display_width(" PgUp/PgDn ") + 2;
            let scroll = display_width(" ↑/↓  scroll") + 2;

            let text = hints(view, wide_width - 1, &[], Quit::Keys);
            assert!(!text.contains("PgUp"), "{view:?}: {text:?}");
            for kept in [
                "←/→  pages",
                "↑/↓  scroll",
                "Esc",
                "?  help",
                "q  exit dashboard",
            ] {
                assert!(text.contains(kept), "{view:?}: lost {kept:?}: {text:?}");
            }
            assert!(text.contains(&identity(view)), "{text:?}");

            let text = hints(view, wide_width - pages - 1, &[], Quit::Keys);
            assert!(!text.contains("↑/↓") && !text.contains("PgUp"), "{text:?}");
            for kept in ["←/→  pages", "Esc  Main", "?  help", "q  exit dashboard"] {
                assert!(text.contains(kept), "{view:?}: lost {kept:?}: {text:?}");
            }
            assert!(text.contains(&identity(view)), "{text:?}");

            let text = hints(view, wide_width - pages - scroll - 1, &[], Quit::Keys);
            assert!(!text.contains("scroll"), "{view:?}: {text:?}");
            for key in KEYS {
                assert!(
                    fields(&text).contains(&key),
                    "{view:?}: lost {key:?}: {text:?}"
                );
            }
        }
    }

    #[test]
    fn a_narrow_footer_keeps_the_pages_arrows_the_way_back_and_help() {
        for view in View::ALL {
            let text = hints(view, 40, &[], Quit::Keys);
            assert!(display_width(&text) <= 40, "{view:?}: {text:?}");
            for key in KEYS {
                assert!(
                    fields(&text).contains(&key),
                    "{view:?}: lost {key:?}: {text:?}"
                );
            }
            assert!(!text.contains("PgUp"), "{text:?}");
        }
    }

    #[test]
    fn all_four_keys_stay_down_to_fourteen_cells_and_q_alone_below() {
        for view in View::ALL {
            for width in 14..=120 {
                let text = hints(view, width, &[], Quit::Keys);
                assert!(display_width(&text) <= width, "{view:?} {width}: {text:?}");
                for key in KEYS {
                    assert!(
                        fields(&text).contains(&key),
                        "{view:?} {width}: lost {key:?}: {text:?}"
                    );
                }
                assert!(!text.contains("..."), "{view:?} {width}: cut {text:?}");
            }
            for width in 1..14 {
                let text = hints(view, width, &[], Quit::Keys);
                assert!(display_width(&text) <= width, "{view:?} {width}: {text:?}");
                assert!(fields(&text).contains(&"q"), "{view:?} {width}: {text:?}");
            }
            assert_eq!(hints(view, 0, &[], Quit::Keys), "");
        }
    }

    #[test]
    fn the_pages_place_outlasts_the_labels_and_goes_before_the_keys() {
        for view in View::ALL {
            let with = |width| hints(view, width, &[], Quit::Keys);
            assert!(with(60).contains(&identity(view)), "{view:?}");
            let short = format!("{}/9", view.number());
            let text = with(25);
            assert!(
                fields(&text).contains(&short.as_str()),
                "{view:?}: {text:?}"
            );
            let text = with(14);
            assert!(!text.contains(&short), "{view:?}: {text:?}");
        }
    }

    #[test]
    fn the_tuning_boot_footer_never_names_a_quit_key() {
        let skipped = [skipped_clause(REPORT, Colours::None)];
        for view in View::ALL {
            for width in 0..=160 {
                for after in [&[][..], &skipped[..]] {
                    let text = hints(view, width, after, Quit::Ignored);
                    assert!(display_width(&text) <= width, "{view:?} {width}: {text:?}");
                    for word in fields(&text) {
                        assert!(
                            !["q", "exit", "dashboard", "close"].contains(&word),
                            "{view:?} {width}: the footer names a quit key: {text:?}"
                        );
                    }
                }
            }
            for width in 14..=120 {
                let text = hints(view, width, &[], Quit::Ignored);
                for key in ["←/→", "Esc", "?"] {
                    assert!(
                        fields(&text).contains(&key),
                        "{view:?} {width}: lost {key:?}: {text:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_skipped_report_follows_the_keys_whole_and_then_as_a_pointer_that_keeps_them() {
        let after = [skipped_clause(REPORT, Colours::None)];
        let wide = hints(View::Main, 300, &after, Quit::Keys);
        assert!(wide.contains(REPORT), "{wide}");
        let narrow = hints(View::Main, 100, &after, Quit::Keys);
        assert!(narrow.contains("togi events"), "{narrow}");
        for key in KEYS {
            assert!(fields(&narrow).contains(&key), "{narrow}");
        }
    }

    #[test]
    fn the_skipped_warning_never_costs_a_key_down_to_seventeen_cells() {
        let after = [skipped_clause(REPORT, Colours::None)];
        for view in View::ALL {
            for width in 17..=160 {
                let text = hints(view, width, &after, Quit::Keys);
                assert!(display_width(&text) <= width, "{view:?} {width}: {text:?}");
                for key in KEYS {
                    assert!(
                        fields(&text).contains(&key),
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
        let text = |width| hints(View::Help, width, &after, Quit::Keys);
        assert!(text(17).ends_with("  !"), "{}", text(17));
        assert!(text(40).ends_with("  skipped"), "{}", text(40));
        assert!(
            text(110).ends_with("  skipped journal content: togi events"),
            "{}",
            text(110)
        );
        assert!(text(300).ends_with(REPORT), "{}", text(300));
    }

    #[test]
    fn the_skipped_warning_leaves_the_close_key_whole_at_any_width() {
        let after = [skipped_clause(REPORT, Colours::None)];
        for view in View::ALL {
            for width in 1..17 {
                let text = hints(view, width, &after, Quit::Keys);
                assert!(display_width(&text) <= width, "{view:?} {width}: {text:?}");
                assert!(fields(&text).contains(&"q"), "{view:?} {width}: {text:?}");
                if width >= 5 {
                    assert!(text.contains('!'), "{view:?} {width}: {text:?}");
                }
            }
            assert_eq!(hints(view, 0, &after, Quit::Keys), "");
        }
    }

    #[test]
    fn the_skipped_warning_keeps_its_amber_and_the_keys_their_style() {
        let after = [skipped_clause(REPORT, Colours::Full)];
        for view in View::ALL {
            let reference = key_hints(view, 300, &[], Colours::Full, Quit::Keys);
            for width in [17, 30, 40, 50] {
                let line = key_hints(view, width, &after, Colours::Full, Quit::Keys);
                for key in KEYS {
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

    #[test]
    fn the_way_back_is_inactive_on_main_and_help_is_current_on_the_help() {
        let style_of = |line: &ratatui::text::Line<'_>, text: &str| {
            line.spans
                .iter()
                .find(|span| span.content == text)
                .map(|span| span.style)
        };
        let main = key_hints(View::Main, 300, &[], Colours::Full, Quit::Keys);
        assert_eq!(
            style_of(&main, " Esc "),
            Some(Paint::Track.style(Colours::Full)),
            "Esc has nothing to go back to on the main page"
        );
        assert_eq!(
            style_of(&main, " ? "),
            Some(Paint::Chip.style(Colours::Full))
        );
        let help = key_hints(View::Help, 300, &[], Colours::Full, Quit::Keys);
        assert_eq!(
            style_of(&help, " ? "),
            Some(Paint::Current.style(Colours::Full))
        );
        assert_eq!(
            style_of(&help, " Esc "),
            Some(Paint::Chip.style(Colours::Full))
        );
        assert_eq!(
            style_of(&help, " q "),
            Some(Paint::Chip.style(Colours::Full))
        );
    }

    #[test]
    fn without_colour_the_footer_carries_no_style() {
        for view in View::ALL {
            let line = key_hints(view, 300, &[], Colours::None, Quit::Keys);
            assert!(line.spans.iter().all(|span| span.style == Style::new()));
            let print = page_names(view, 300, &[], Colours::None);
            assert!(print.spans.iter().all(|span| span.style == Style::new()));
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
        let text = |width| hints(View::Log, width, &arrival, Quit::Keys);
        assert_eq!(
            text(300),
            format!(
                "{}   3 new events · End: latest",
                full(View::Log, Quit::Keys)
            )
        );
        for width in 17..=120 {
            let text = text(width);
            for key in KEYS {
                assert!(
                    fields(&text).contains(&key),
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
        assert!(!text(17).contains("new"), "{}", text(17));
    }

    fn draw_footer(
        view: View,
        width: u16,
        height: u16,
        colours: Colours,
        keys: bool,
    ) -> (Buffer, Rect) {
        let sc = screen(view, width, height, colours, keys);
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
    fn the_drawn_footer_follows_the_screens_quit_policy_on_every_page() {
        for view in View::ALL {
            for quit in [Quit::Keys, Quit::Ignored] {
                let sc = Screen {
                    quit,
                    ..screen(view, 200, 50, Colours::None, true)
                };
                let (buf, _) = draw_checked(&sc, |sc, buf| {
                    footer(sc, &[], buf);
                    Drawn {
                        scroll: 0,
                        hold: Hold::Never,
                    }
                });
                let text = footer_text(&buf, Layout::new(&sc).footer);
                assert_eq!(text, full(view, quit), "{view:?} {quit:?}");
            }
        }
    }

    #[test]
    fn a_print_shows_the_pages_place_and_names_and_no_key_to_press() {
        for view in View::ALL {
            let (buf, rect) = draw_footer(view, 200, 50, Colours::None, false);
            let text = footer_text(&buf, rect);
            assert!(text.contains(&identity(view)), "{view:?}: {text:?}");
            for page in View::ALL {
                let name = if page == view {
                    format!("[{}]", page.name())
                } else {
                    page.name().to_owned()
                };
                assert!(text.contains(&name), "{view:?}: lost {name:?}: {text:?}");
            }
            for invitation in ["Esc", "PgUp", "scroll", "exit", "?", "←", "↑"] {
                assert!(
                    !text.contains(invitation),
                    "{view:?}: a print invites {invitation:?}: {text:?}"
                );
            }
        }
    }

    #[test]
    fn a_narrow_print_keeps_the_pages_place_longest() {
        for view in View::ALL {
            let short = format!("{}/9", view.number());
            for width in 1..=200 {
                let text = plain(&page_names(view, width, &[], Colours::None));
                assert!(display_width(&text) <= width, "{view:?} {width}: {text:?}");
                assert!(!text.contains("..."), "{view:?} {width}: {text:?}");
                if width >= display_width(&identity(view)) {
                    assert!(text.contains(&identity(view)), "{view:?} {width}: {text:?}");
                } else if width >= 3 {
                    assert!(text.contains(&short), "{view:?} {width}: {text:?}");
                }
            }
            let names = plain(&page_names(view, 200, &[], Colours::None));
            assert!(names.contains("Hunts / probes"), "{names:?}");
        }
    }

    #[test]
    fn a_print_follows_its_place_with_the_skipped_report() {
        let after = [skipped_clause(REPORT, Colours::None)];
        let text = plain(&page_names(View::Cores, 300, &after, Colours::None));
        assert!(text.starts_with("2/9 Cores"), "{text:?}");
        assert!(text.ends_with(REPORT), "{text:?}");
        let narrow = plain(&page_names(View::Cores, 30, &after, Colours::None));
        assert!(narrow.starts_with("2/9 Cores"), "{narrow:?}");
        assert!(narrow.contains("skipped"), "{narrow:?}");
    }

    #[test]
    fn the_footer_draws_only_console_glyphs() {
        for view in View::ALL {
            for keys in [true, false] {
                let (buf, _) = draw_footer(view, 240, 50, Colours::Full, keys);
                assert_glyphs_drawable(&buf, &format!("footer {view:?} keys {keys}"));
            }
        }
    }

    #[test]
    fn the_footer_stays_in_bounds_at_every_size() {
        for view in [View::Main, View::Cores, View::Log, View::Help] {
            for width in 0..=60 {
                for height in 0..=14 {
                    for colours in [Colours::Full, Colours::Console, Colours::None] {
                        for keys in [true, false] {
                            let sc = screen(view, width, height, colours, keys);
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
    }
}
