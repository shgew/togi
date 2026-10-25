//! Colour profiles: the paints the panels draw with, and how each profile applies them.
//!
//! Every paint names a foreground and an optional background in the 16 slots.
//! The Linux console redefines those slots with [`console_palette`]; rich
//! terminals receive the same RGB values directly. No paint uses attributes.
//! The model's semantic [`Tone`](crate::words::Tone) becomes a paint through
//! one `From`.

use std::borrow::Cow;
use std::ffi::OsStr;

use ratatui::style::{Color, Style};
use ratatui::text::Span;
use togi_render::OutputFacts;

use crate::screen::Colours;
use crate::words::Tone;

/// The drawing role of a piece of text on the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Paint {
    /// Running prose.
    Text,
    /// Idle, parked, still to come, and the dim rest.
    Grey,
    /// A name or value that stands out.
    White,
    /// What runs now.
    Running,
    /// A hold, a veto, a retreat, a crash: attention without a failure.
    Warn,
    /// Passed, done.
    Good,
    /// Failures, past and present.
    Bad,
    /// Rules and unfilled tracks.
    Track,
    /// A key in the footer.
    Chip,
    /// The lamp of a running trial.
    Lamp,
    /// The footer key of the view on screen.
    Current,
}

impl Paint {
    /// Every paint, for tests and legends.
    pub const ALL: [Self; 11] = [
        Self::Text,
        Self::Grey,
        Self::White,
        Self::Running,
        Self::Warn,
        Self::Good,
        Self::Bad,
        Self::Track,
        Self::Chip,
        Self::Lamp,
        Self::Current,
    ];

    /// How `colours` paints this tone. No tone carries an attribute.
    #[must_use]
    pub fn style(self, colours: Colours) -> Style {
        let fg = |color| Style::new().fg(color);
        let (foreground, background) = match self {
            Self::Text => (7, None),
            Self::Grey => (8, None),
            Self::White => (15, None),
            Self::Running => (14, None),
            Self::Warn => (11, None),
            Self::Good => (10, None),
            Self::Bad => (9, None),
            Self::Track => (5, None),
            Self::Chip => (15, Some(5)),
            Self::Lamp => (0, Some(3)),
            Self::Current => (0, Some(7)),
        };
        let color = |slot| match colours {
            Colours::Rich => {
                let (r, g, b) = console_palette()[slot];
                Color::Rgb(r, g, b)
            }
            Colours::Full | Colours::Console | Colours::None => SLOTS[slot],
        };
        let mut style = fg(color(foreground));
        if let Some(background) = background {
            style = style.bg(color(background));
        }
        match colours {
            Colours::Full | Colours::Console | Colours::Rich => style,
            Colours::None => Style::new(),
        }
    }

    /// `text` in this paint.
    #[must_use]
    pub fn span(self, colours: Colours, text: impl Into<Cow<'static, str>>) -> Span<'static> {
        Span::styled(text, self.style(colours))
    }
}

impl From<Tone> for Paint {
    fn from(tone: Tone) -> Self {
        match tone {
            Tone::Plain => Self::Text,
            Tone::Good => Self::Good,
            Tone::Bad => Self::Bad,
            Tone::Warn => Self::Warn,
        }
    }
}

/// The output's profile. Plain output wins over terminal capabilities, and the Linux console always uses its
/// own palette and glyphs. Other terminals opt into rich rendering by advertising truecolour.
#[must_use]
pub fn colours_for(facts: OutputFacts, term: Option<&OsStr>, colorterm: Option<&OsStr>) -> Colours {
    if facts.no_color || facts.journald || !facts.is_terminal {
        Colours::None
    } else if term.is_some_and(|term| term == "linux") {
        Colours::Console
    } else if colorterm.is_some_and(|value| value == "truecolor" || value == "24bit") {
        Colours::Rich
    } else {
        Colours::Full
    }
}

const SLOTS: [Color; 16] = [
    Color::Black,
    Color::Red,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::Gray,
    Color::DarkGray,
    Color::LightRed,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightCyan,
    Color::White,
];

/// The dashboard's shade for each of the 16 colour slots on the Linux console, which allows redefining them.
/// The console's own greys and bold handling are too dim at low monitor brightness. The loop writes the escape
/// sequences and restores the console's palette on exit.
#[must_use]
pub const fn console_palette() -> [(u8, u8, u8); 16] {
    [
        (0x00, 0x00, 0x00),
        (0xaa, 0x33, 0x33),
        (0x33, 0xaa, 0x55),
        (0xaa, 0x77, 0x22),
        (0x2a, 0x4a, 0x80),
        (0x43, 0x4a, 0x54),
        (0x2a, 0x8a, 0x8a),
        (0xe4, 0xe4, 0xe4),
        (0x8b, 0x91, 0x9b),
        (0xff, 0x6b, 0x6b),
        (0x6b, 0xe0, 0x8a),
        (0xff, 0xc0, 0x4d),
        (0x7f, 0xb4, 0xff),
        (0xd7, 0x9b, 0xff),
        (0x5f, 0xe3, 0xe3),
        (0xff, 0xff, 0xff),
    ]
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use ratatui::style::{Color, Modifier, Style};
    use togi_render::OutputFacts;

    use super::{Paint, colours_for, console_palette};
    use crate::screen::Colours;

    fn terminal() -> OutputFacts {
        OutputFacts {
            is_terminal: true,
            no_color: false,
            journald: false,
        }
    }

    fn slot(color: Option<Color>) -> Option<u8> {
        Some(match color? {
            Color::Black => 0,
            Color::Red => 1,
            Color::Green => 2,
            Color::Yellow => 3,
            Color::Blue => 4,
            Color::Magenta => 5,
            Color::Cyan => 6,
            Color::Gray => 7,
            Color::DarkGray => 8,
            Color::LightRed => 9,
            Color::LightGreen => 10,
            Color::LightYellow => 11,
            Color::LightBlue => 12,
            Color::LightMagenta => 13,
            Color::LightCyan => 14,
            Color::White => 15,
            other @ (Color::Reset | Color::Rgb(..) | Color::Indexed(_)) => {
                panic!("tone leaves the 16 slots: {other:?}")
            }
        })
    }

    #[test]
    fn every_tone_stays_in_the_sixteen_slots_with_backgrounds_in_the_first_eight() {
        for colours in [Colours::Full, Colours::Console] {
            for tone in Paint::ALL {
                let style = tone.style(colours);
                assert!(
                    slot(style.fg).is_some(),
                    "{colours:?} {tone:?} has a foreground"
                );
                if let Some(bg) = slot(style.bg) {
                    assert!(
                        bg < 8,
                        "{colours:?} {tone:?} background slot {bg} is a bright one"
                    );
                }
            }
        }
    }

    #[test]
    fn the_console_profile_uses_no_attribute_and_no_colour_paints_nothing() {
        for tone in Paint::ALL {
            let console = tone.style(Colours::Console);
            assert_eq!(
                console.add_modifier,
                Modifier::empty(),
                "{tone:?} on the console"
            );
            assert_eq!(
                console.sub_modifier,
                Modifier::empty(),
                "{tone:?} on the console"
            );
            assert_eq!(
                tone.style(Colours::None),
                Style::new(),
                "{tone:?} without colour"
            );
        }
    }

    #[test]
    fn full_colour_and_the_console_paint_the_same_slots() {
        for tone in Paint::ALL {
            assert_eq!(
                tone.style(Colours::Full),
                tone.style(Colours::Console),
                "{tone:?}"
            );
        }
    }

    #[test]
    fn tones_are_told_apart_by_style() {
        for (i, a) in Paint::ALL.iter().enumerate() {
            for b in &Paint::ALL[i + 1..] {
                assert_ne!(
                    a.style(Colours::Full),
                    b.style(Colours::Full),
                    "{a:?} and {b:?}"
                );
            }
        }
    }

    #[test]
    fn a_span_carries_its_tone() {
        let span = Paint::Bad.span(Colours::Full, "█");
        assert_eq!(span.content, "█");
        assert_eq!(span.style, Paint::Bad.style(Colours::Full));
    }

    #[test]
    fn colours_follow_the_output_facts_and_term() {
        let linux = Some(OsStr::new("linux"));
        let xterm = Some(OsStr::new("xterm-256color"));
        assert_eq!(colours_for(terminal(), linux, None), Colours::Console);
        assert_eq!(colours_for(terminal(), xterm, None), Colours::Full);
        assert_eq!(colours_for(terminal(), None, None), Colours::Full);
        let no_color = OutputFacts {
            no_color: true,
            ..terminal()
        };
        assert_eq!(colours_for(no_color, linux, None), Colours::None);
        let journald = OutputFacts {
            journald: true,
            ..terminal()
        };
        assert_eq!(colours_for(journald, xterm, None), Colours::None);
        let piped = OutputFacts {
            is_terminal: false,
            ..terminal()
        };
        assert_eq!(colours_for(piped, linux, None), Colours::None);
    }

    #[test_case::test_case(Some("xterm-256color"), Some("truecolor"), true, false, false, Colours::Rich; "truecolor")]
    #[test_case::test_case(Some("xterm-256color"), Some("24bit"), true, false, false, Colours::Rich; "24bit")]
    #[test_case::test_case(None, Some("truecolor"), true, false, false, Colours::Rich; "term absent")]
    #[test_case::test_case(Some("linux"), Some("truecolor"), true, false, false, Colours::Console; "console wins")]
    #[test_case::test_case(Some("xterm"), Some("truecolor"), true, true, false, Colours::None; "no color wins")]
    #[test_case::test_case(Some("xterm"), Some("truecolor"), true, false, true, Colours::None; "journald wins")]
    #[test_case::test_case(Some("xterm"), Some("truecolor"), false, false, false, Colours::None; "pipe wins")]
    #[test_case::test_case(Some("xterm"), Some("TRUECOLOR"), true, false, false, Colours::Full; "unknown advertisement")]
    fn rich_requires_a_truecolour_terminal(
        term: Option<&str>,
        colorterm: Option<&str>,
        is_terminal: bool,
        no_color: bool,
        journald: bool,
        wanted: Colours,
    ) {
        assert_eq!(
            colours_for(
                OutputFacts {
                    is_terminal,
                    no_color,
                    journald
                },
                term.map(OsStr::new),
                colorterm.map(OsStr::new),
            ),
            wanted
        );
    }

    #[test]
    fn rich_paints_the_exact_console_rgb_for_foregrounds_and_backgrounds_without_attributes() {
        let rgb = |color| {
            slot(color).map(|slot| {
                let (r, g, b) = console_palette()[usize::from(slot)];
                Color::Rgb(r, g, b)
            })
        };
        for paint in Paint::ALL {
            let console = paint.style(Colours::Console);
            let rich = paint.style(Colours::Rich);
            assert_eq!(rich.fg, rgb(console.fg), "{paint:?} foreground");
            assert_eq!(rich.bg, rgb(console.bg), "{paint:?} background");
            assert_eq!(rich.add_modifier, Modifier::empty(), "{paint:?}");
            assert_eq!(rich.sub_modifier, Modifier::empty(), "{paint:?}");
        }
    }

    #[test]
    fn the_console_palette_keeps_the_slots_the_frames_rely_on() {
        let palette = console_palette();
        assert_eq!(palette[0], (0, 0, 0));
        assert_eq!(palette[15], (255, 255, 255));
        let luma = |(r, g, b): (u8, u8, u8)| u32::from(r) * 3 + u32::from(g) * 6 + u32::from(b);
        assert!(
            luma(palette[8]) > luma(palette[5]),
            "grey text must stand out from the track"
        );
        assert!(
            luma(palette[7]) > luma(palette[8]),
            "text must be lighter than grey"
        );
        assert!(
            luma(palette[15]) > luma(palette[7]),
            "white must be lighter than text"
        );
        let mut distinct = palette.to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), 16, "every slot has its own shade");
    }
}
