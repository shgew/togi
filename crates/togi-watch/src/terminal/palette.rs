//! The colour profile of an output, and the Linux console's palette.

use std::ffi::OsStr;

use togi_render::OutputFacts;

use crate::screen::Colours;

/// The dashboard's shade for each of the console's 16 colour slots, which the console lets a program redefine
/// (`ESC ] P n rrggbb`). The console's default grey and its bold handling are too dim at low monitor brightness.
pub const CONSOLE_PALETTE: [(u8, u8, u8); 16] = [
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
];

/// Resets the console's palette to its defaults.
pub(super) const RESET: &str = "\x1b]R";

/// The sequence that sets [`CONSOLE_PALETTE`].
pub(super) fn set() -> String {
    use std::fmt::Write as _;
    let mut sequence = String::new();
    for (slot, (red, green, blue)) in CONSOLE_PALETTE.iter().enumerate() {
        let _ = write!(sequence, "\x1b]P{slot:X}{red:02x}{green:02x}{blue:02x}");
    }
    sequence
}

/// The colour profile of an output: none for `NO_COLOR` or journald, the console's 16 colours for `TERM=linux`,
/// every colour otherwise.
pub fn colours_for(facts: OutputFacts, term: Option<&OsStr>) -> Colours {
    if facts.no_color || facts.journald {
        Colours::None
    } else if term == Some(OsStr::new("linux")) {
        Colours::Console
    } else {
        Colours::Full
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use togi_render::OutputFacts;

    use super::{CONSOLE_PALETTE, colours_for, set};
    use crate::screen::Colours;

    fn facts(no_color: bool, journald: bool) -> OutputFacts {
        OutputFacts {
            is_terminal: true,
            no_color,
            journald,
        }
    }

    #[test]
    fn the_palette_sets_every_slot_in_hex() {
        let set = set();
        assert_eq!(set.matches("\x1b]P").count(), 16);
        assert!(set.starts_with("\x1b]P0000000\x1b]P1aa3333"));
        assert!(set.ends_with("\x1b]PFffffff"));
        assert_eq!(CONSOLE_PALETTE.len(), 16);
    }

    #[test]
    fn colours_follow_no_color_journald_and_term() {
        let linux = Some(OsStr::new("linux"));
        let xterm = Some(OsStr::new("xterm-256color"));
        assert_eq!(colours_for(facts(false, false), xterm), Colours::Full);
        assert_eq!(colours_for(facts(false, false), None), Colours::Full);
        assert_eq!(colours_for(facts(false, false), linux), Colours::Console);
        assert_eq!(colours_for(facts(true, false), xterm), Colours::None);
        assert_eq!(colours_for(facts(true, false), linux), Colours::None);
        assert_eq!(colours_for(facts(false, true), linux), Colours::None);
    }
}
