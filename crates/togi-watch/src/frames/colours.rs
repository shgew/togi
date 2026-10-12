//! Colour profiles: the paints the panels draw with, and how each profile applies them.
//!
//! Every paint is a foreground and an optional background in the 16 slots of the
//! terminal. Slots are named, not RGB, so the Linux console can redefine them
//! with [`console_palette`]. No paint uses an attribute: bold would change the
//! foreground slot on `TERM=linux`, so the console never gets one. The model's
//! semantic [`Tone`](crate::words::Tone) becomes a paint through one `From`.

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
    /// A hunt, and what it paused.
    Hunt,
    /// Passed, done.
    Good,
    /// Failures, past and present.
    Bad,
    /// Combinations.
    Combination,
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
    pub const ALL: [Self; 12] = [
        Self::Text,
        Self::Grey,
        Self::White,
        Self::Running,
        Self::Hunt,
        Self::Good,
        Self::Bad,
        Self::Combination,
        Self::Track,
        Self::Chip,
        Self::Lamp,
        Self::Current,
    ];

    /// How `colours` paints this tone: nothing without colour, slots otherwise. No tone carries an attribute,
    /// so the console never gets bold.
    #[must_use]
    pub fn style(self, colours: Colours) -> Style {
        let fg = |color| Style::new().fg(color);
        let style = match self {
            Self::Text => fg(Color::Gray),
            Self::Grey => fg(Color::DarkGray),
            Self::White => fg(Color::White),
            Self::Running => fg(Color::LightCyan),
            Self::Hunt => fg(Color::LightYellow),
            Self::Good => fg(Color::LightGreen),
            Self::Bad => fg(Color::LightRed),
            Self::Combination => fg(Color::LightMagenta),
            Self::Track => fg(Color::Magenta),
            Self::Chip => fg(Color::White).bg(Color::Magenta),
            Self::Lamp => fg(Color::Black).bg(Color::Yellow),
            Self::Current => fg(Color::Black).bg(Color::Gray),
        };
        match colours {
            Colours::Full | Colours::Console => style,
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
            Tone::Warn | Tone::Hunt => Self::Hunt,
            Tone::Combo => Self::Combination,
        }
    }
}

/// The colour profile of an output: none for `NO_COLOR`, journald and anything that is not a terminal, the
/// 16-colour console for `TERM=linux`, full colour otherwise.
#[must_use]
pub fn colours_for(facts: OutputFacts, term: Option<&OsStr>) -> Colours {
    if facts.no_color || facts.journald || !facts.is_terminal {
        Colours::None
    } else if term.is_some_and(|term| term == "linux") {
        Colours::Console
    } else {
        Colours::Full
    }
}

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
            other => panic!("tone leaves the 16 slots: {other:?}"),
        })
    }

    #[test]
    fn every_tone_stays_in_the_sixteen_slots_with_backgrounds_in_the_first_eight() {
        for colours in [Colours::Full, Colours::Console] {
            for tone in Paint::ALL {
                let style = tone.style(colours);
                assert!(slot(style.fg).is_some(), "{tone:?} has a foreground");
                if let Some(bg) = slot(style.bg) {
                    assert!(bg < 8, "{tone:?} background slot {bg} is a bright one");
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
        assert_eq!(colours_for(terminal(), linux), Colours::Console);
        assert_eq!(colours_for(terminal(), xterm), Colours::Full);
        assert_eq!(colours_for(terminal(), None), Colours::Full);
        let no_color = OutputFacts {
            no_color: true,
            ..terminal()
        };
        assert_eq!(colours_for(no_color, linux), Colours::None);
        let journald = OutputFacts {
            journald: true,
            ..terminal()
        };
        assert_eq!(colours_for(journald, xterm), Colours::None);
        let piped = OutputFacts {
            is_terminal: false,
            ..terminal()
        };
        assert_eq!(colours_for(piped, linux), Colours::None);
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
