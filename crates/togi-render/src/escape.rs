use std::fmt;

#[derive(Clone, Copy, Debug)]
pub struct Escaped<'a> {
    bytes: &'a [u8],
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

    fn text(input: &str) -> String {
        escape_text(input).to_string()
    }

    fn bytes(input: &[u8]) -> String {
        escape_bytes(input).to_string()
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

    #[test]
    fn range_boundaries_escape_exactly_the_listed_characters() {
        for escaped in [
            '\u{61c}', '\u{200e}', '\u{200f}', '\u{2028}', '\u{2029}', '\u{202a}', '\u{202b}',
            '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            assert_eq!(
                text(&escaped.to_string()),
                format!("\\u{:04x}", u32::from(escaped))
            );
        }
        for readable in [
            '\u{7e}', '\u{a0}', '\u{61b}', '\u{61d}', '\u{200d}', '\u{2010}', '\u{2027}',
            '\u{202f}', '\u{2065}', '\u{206a}', '\u{feff}',
        ] {
            assert_eq!(text(&readable.to_string()), readable.to_string());
        }
    }

    #[test]
    fn readable_unicode_and_visible_escapes_are_untouched() {
        for input in [
            "trial 0413 PASS | Tctl max 71°C",
            "日本語 🧪 ünï",
            "back\\slash \\x1b \\n",
            "",
        ] {
            assert_eq!(text(input), input);
        }
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
