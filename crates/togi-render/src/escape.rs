use std::borrow::Cow;
use std::fmt;

#[derive(Clone, Copy, Debug)]
pub struct Escaped<'a> {
    bytes: &'a [u8],
}

impl<'a> Escaped<'a> {
    /// Escaped text, borrowing valid UTF-8 that needs no escaping.
    #[must_use]
    pub fn into_cow(self) -> Cow<'a, str> {
        if let Ok(text) = std::str::from_utf8(self.bytes)
            && !text.chars().any(needs_escape)
        {
            return Cow::Borrowed(text);
        }
        Cow::Owned(self.to_string())
    }
}

#[must_use]
pub fn escape_text(text: &str) -> Escaped<'_> {
    Escaped {
        bytes: text.as_bytes(),
    }
}

#[must_use]
pub fn escape_bytes(bytes: &[u8]) -> Escaped<'_> {
    Escaped { bytes }
}

const fn needs_escape(c: char) -> bool {
    matches!(
        c,
        '\u{0}'..='\u{1f}'
            | '\u{7f}'..='\u{9f}'
            | '\u{61c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn write_char(f: &mut fmt::Formatter<'_>, c: char) -> fmt::Result {
    match c {
        '\t' => f.write_str("\\t"),
        '\n' => f.write_str("\\n"),
        '\r' => f.write_str("\\r"),
        '\u{0}'..='\u{7f}' => write!(f, "\\x{:02x}", u32::from(c)),
        _ => write!(f, "\\u{:04x}", u32::from(c)),
    }
}

fn write_str(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    let mut run = 0;
    for (index, c) in text.char_indices() {
        if needs_escape(c) {
            f.write_str(&text[run..index])?;
            write_char(f, c)?;
            run = index + c.len_utf8();
        }
    }
    f.write_str(&text[run..])
}

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.bytes.utf8_chunks() {
            write_str(f, chunk.valid())?;
            for byte in chunk.invalid() {
                write!(f, "\\x{byte:02x}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{escape_bytes, escape_text};
    use test_case::test_case;

    fn text(input: &str) -> String {
        escape_text(input).to_string()
    }

    fn bytes(input: &[u8]) -> String {
        escape_bytes(input).to_string()
    }

    #[test_case("▆ … 日本語 e\u{301}", true, "▆ … 日本語 e\u{301}"; "safe Unicode is borrowed")]
    #[test_case("a\x1b[2J", false, r"a\x1b[2J"; "terminal command")]
    #[test_case("a\u{202e}b", false, r"a\u202eb"; "bidirectional control")]
    fn owned_escaping_allocates_only_when_text_changes(
        input: &str,
        borrowed: bool,
        expected: &str,
    ) {
        let escaped = escape_text(input).into_cow();
        assert_eq!(matches!(escaped, std::borrow::Cow::Borrowed(_)), borrowed);
        assert_eq!(escaped, expected);
    }

    #[test]
    fn owned_escaping_keeps_invalid_bytes_visible() {
        assert_eq!(escape_bytes(b"a\xffb").into_cow(), r"a\xffb");
    }

    #[test]
    fn terminal_commands_become_visible_text() {
        assert_eq!(text("\x1b[31mred\x1b[0m"), "\\x1b[31mred\\x1b[0m");
        assert_eq!(text("\x1b]0;title\x07"), "\\x1b]0;title\\x07");
        assert_eq!(text("\x1b]0;title\x1b\\"), "\\x1b]0;title\\x1b\\");
        assert_eq!(text("a\u{9b}31m\u{9d}x"), "a\\u009b31m\\u009dx");
    }

    #[test]
    fn whitespace_controls_use_short_escapes() {
        assert_eq!(text("a\tb\nc\rd"), "a\\tb\\nc\\rd");
    }

    #[test]
    fn every_c0_control_and_del_is_escaped() {
        for value in 0u8..=0x1f {
            let character = char::from(value);
            let expected = match character {
                '\t' => "\\t".to_owned(),
                '\n' => "\\n".to_owned(),
                '\r' => "\\r".to_owned(),
                _ => format!("\\x{value:02x}"),
            };
            assert_eq!(text(&character.to_string()), expected);
        }
        assert_eq!(text("\x7f"), "\\x7f");
    }

    #[test]
    fn every_c1_control_uses_four_digit_escapes() {
        for value in 0x80u32..=0x9f {
            let character = char::from_u32(value).unwrap();
            assert_eq!(text(&character.to_string()), format!("\\u{value:04x}"));
        }
    }

    #[test_case('\u{61c}'; "arabic letter mark")]
    #[test_case('\u{200e}'; "left-to-right mark")]
    #[test_case('\u{200f}'; "right-to-left mark")]
    #[test_case('\u{2028}'; "line separator")]
    #[test_case('\u{2029}'; "paragraph separator")]
    #[test_case('\u{202a}'; "left-to-right embedding")]
    #[test_case('\u{202b}'; "right-to-left embedding")]
    #[test_case('\u{202c}'; "pop directional formatting")]
    #[test_case('\u{202d}'; "left-to-right override")]
    #[test_case('\u{202e}'; "right-to-left override")]
    #[test_case('\u{2066}'; "left-to-right isolate")]
    #[test_case('\u{2067}'; "right-to-left isolate")]
    #[test_case('\u{2068}'; "first strong isolate")]
    #[test_case('\u{2069}'; "pop directional isolate")]
    fn range_boundary_escapes_listed_character(escaped: char) {
        assert_eq!(
            text(&escaped.to_string()),
            format!("\\u{:04x}", u32::from(escaped))
        );
    }

    #[test_case('\u{7e}'; "tilde")]
    #[test_case('\u{a0}'; "no-break space")]
    #[test_case('\u{61b}'; "arabic semicolon")]
    #[test_case('\u{61d}'; "arabic end of text mark")]
    #[test_case('\u{200d}'; "zero width joiner")]
    #[test_case('\u{2010}'; "hyphen")]
    #[test_case('\u{2027}'; "hyphenation point")]
    #[test_case('\u{202f}'; "narrow no-break space")]
    #[test_case('\u{2065}'; "unassigned before isolates")]
    #[test_case('\u{206a}'; "inhibit symmetric swapping")]
    #[test_case('\u{feff}'; "byte order mark")]
    fn range_boundary_leaves_neighbouring_character_readable(readable: char) {
        assert_eq!(text(&readable.to_string()), readable.to_string());
    }

    #[test_case("trial 0413 PASS | Tctl max 71°C"; "status line")]
    #[test_case("日本語 🧪 ünï"; "unicode")]
    #[test_case("back\\slash \\x1b \\n"; "visible escapes")]
    #[test_case(""; "empty")]
    fn readable_unicode_and_visible_escapes_are_untouched(input: &str) {
        assert_eq!(text(input), input);
    }

    #[test]
    fn invalid_utf8_escapes_each_byte() {
        assert_eq!(bytes(b"a\x80b"), "a\\x80b");
        assert_eq!(bytes(b"\xff\xfe"), "\\xff\\xfe");
        assert_eq!(bytes(b"\x9b31m"), "\\x9b31m");
        assert_eq!(bytes(b"\xc3"), "\\xc3");
        assert_eq!(bytes(b"\xc0\xaf"), "\\xc0\\xaf");
        assert_eq!(bytes(b"\xed\xa0\x80"), "\\xed\\xa0\\x80");
        assert_eq!(bytes(b"\xf4\x90\x80\x80"), "\\xf4\\x90\\x80\\x80");
        assert_eq!(bytes(b"ok\xe2\x82"), "ok\\xe2\\x82");
        assert_eq!(bytes(b"\xf0\x9f\x9f"), "\\xf0\\x9f\\x9f");
    }

    #[test]
    fn invalid_bytes_between_valid_text_keep_the_text_and_escape_controls() {
        assert_eq!(bytes(b"a\x1b\xffb\xc3\xa9\n"), "a\\x1b\\xffb\u{e9}\\n");
    }

    #[test]
    fn a_genuine_replacement_character_stays_readable() {
        assert_eq!(text("a\u{fffd}b"), "a\u{fffd}b");
        assert_eq!(bytes("a\u{fffd}b".as_bytes()), "a\u{fffd}b");
        assert_eq!(bytes(b"\xef\xbf\xbd\xff"), "\u{fffd}\\xff");
    }

    #[test]
    fn valid_bytes_match_text() {
        let input = "x\u{202e}y\u{9d}\u{85}";
        assert_eq!(bytes(input.as_bytes()), text(input));
    }

    #[test]
    fn display_streams_into_a_larger_format() {
        assert_eq!(format!("[{}]", escape_text("a\x00")), "[a\\x00]");
    }
}
