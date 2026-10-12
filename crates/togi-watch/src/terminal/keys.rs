//! Keys: decoding the raw bytes a terminal sends, and what a key does to the screen.

use crate::screen::{Screen, View};

/// One key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Key {
    /// A printable or control character (a byte of other input maps to the character of the same value).
    Char(char),
    Esc,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// The most bytes an unfinished sequence may hold before it is dropped.
const PENDING_LIMIT: usize = 128;

/// The key an escape sequence names, from the bytes after the escape: the ones the Linux console and common
/// terminals send for the keys that scroll.
fn sequence_key(sequence: &[u8]) -> Option<Key> {
    match sequence {
        b"[A" | b"OA" => Some(Key::Up),
        b"[B" | b"OB" => Some(Key::Down),
        b"[5~" => Some(Key::PageUp),
        b"[6~" => Some(Key::PageDown),
        b"[H" | b"OH" | b"[1~" | b"[7~" => Some(Key::Home),
        b"[F" | b"OF" | b"[4~" | b"[8~" => Some(Key::End),
        _ => None,
    }
}

/// Splits `bytes` into keys and says how many bytes it used. Unless `last`, an escape or an escape sequence that
/// is not finished stays unused.
fn split(bytes: &[u8], last: bool) -> (Vec<Key>, usize) {
    let mut keys = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != 0x1b {
            keys.push(Key::Char(char::from(bytes[at])));
            at += 1;
            continue;
        }
        let Some(&next) = bytes.get(at + 1) else {
            if !last {
                return (keys, at);
            }
            keys.push(Key::Esc);
            at += 1;
            continue;
        };
        if next != b'[' && next != b'O' {
            keys.push(Key::Esc);
            at += 1;
            continue;
        }
        let Some(offset) = bytes[at + 2..]
            .iter()
            .position(|byte| (0x40..=0x7e).contains(byte))
        else {
            if !last {
                return (keys, at);
            }
            return (keys, bytes.len());
        };
        let end = at + 2 + offset;
        if let Some(key) = sequence_key(&bytes[at + 1..=end]) {
            keys.push(key);
        }
        at = end + 1;
    }
    (keys, bytes.len())
}

/// Decodes a complete batch of input.
#[cfg(test)]
pub(super) fn decode(bytes: &[u8]) -> Vec<Key> {
    split(bytes, true).0
}

/// Decodes a stream, holding back an unfinished escape sequence until its rest arrives or the stream goes quiet.
#[derive(Debug, Default)]
pub(super) struct Decoder {
    pending: Vec<u8>,
}

impl Decoder {
    pub(super) fn push(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(bytes);
        let (keys, used) = split(&self.pending, false);
        self.pending.drain(..used);
        if self.pending.len() >= PENDING_LIMIT {
            self.pending.clear();
        }
        keys
    }

    pub(super) fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Decodes what is held as final: a lone escape is the Esc key, an unfinished sequence is dropped.
    pub(super) fn flush(&mut self) -> Vec<Key> {
        let (keys, _) = split(&self.pending, true);
        self.pending.clear();
        keys
    }
}

fn main_view(screen: Screen) -> Screen {
    Screen {
        view: View::Main,
        scroll: 0,
        ..screen
    }
}

fn view(screen: Screen, view: View) -> Screen {
    Screen {
        view,
        scroll: if view == View::Log { usize::MAX } else { 0 },
        ..screen
    }
}

/// Applies a key to the screen: `?` and `h` toggle the help and `l` the event log, Esc returns to the main view,
/// the arrow, page, Home and End keys (or `k`, `j`, `b`, space, `f`, `g` and `G`) scroll a view that can scroll up
/// to `scrolled` lines, and `q`, Ctrl-C and Ctrl-D quit. The help opens at its top and the log at its end, where it
/// keeps following new events (`usize::MAX`) until a scroll leaves the end. Reports whether to quit.
pub(super) fn press(screen: Screen, key: Key, scrolled: usize) -> (Screen, bool) {
    let page = usize::from(screen.height.saturating_sub(8)).max(1);
    let at = screen.scroll.min(scrolled);
    let to = |line: usize| {
        if screen.view == View::Main {
            return screen;
        }
        let scroll = line.min(scrolled);
        Screen {
            scroll: if scroll == scrolled && screen.view == View::Log {
                usize::MAX
            } else {
                scroll
            },
            ..screen
        }
    };
    match key {
        Key::Char('q' | 'Q' | '\x03' | '\x04') => (screen, true),
        Key::Char('?' | 'h' | 'H') => (
            if screen.view == View::Help {
                main_view(screen)
            } else {
                view(screen, View::Help)
            },
            false,
        ),
        Key::Char('l' | 'L') => (
            if screen.view == View::Log {
                main_view(screen)
            } else {
                view(screen, View::Log)
            },
            false,
        ),
        Key::Esc => (main_view(screen), false),
        Key::Up | Key::Char('k') => (to(at.saturating_sub(1)), false),
        Key::Down | Key::Char('j') => (to(at.saturating_add(1)), false),
        Key::PageUp | Key::Char('b') => (to(at.saturating_sub(page)), false),
        Key::PageDown | Key::Char(' ' | 'f') => (to(at.saturating_add(page)), false),
        Key::Home | Key::Char('g') => (to(0), false),
        Key::End | Key::Char('G') => (to(scrolled), false),
        Key::Char(_) => (screen, false),
    }
}

#[cfg(test)]
mod tests {
    use super::{Decoder, Key, decode, press};
    use crate::screen::{Colours, Screen, View};

    fn screen(view: View, scroll: usize, height: u16) -> Screen {
        Screen {
            view,
            keys: true,
            scroll,
            width: 80,
            height,
            one_frame: false,
            colours: Colours::None,
        }
    }

    struct Case {
        name: &'static str,
        from: Screen,
        key: Key,
        scrolled: usize,
        view: View,
        scroll: usize,
    }

    #[test]
    fn press_maps_every_key_of_go_test_press() {
        let follow = usize::MAX;
        let cases = [
            Case { name: "? opens the help at its top", from: screen(View::Main, 3, 0), key: Key::Char('?'), scrolled: 0, view: View::Help, scroll: 0 },
            Case { name: "? closes the help", from: screen(View::Help, 4, 0), key: Key::Char('?'), scrolled: 9, view: View::Main, scroll: 0 },
            Case { name: "L opens the log at its end", from: screen(View::Main, 0, 0), key: Key::Char('L'), scrolled: 0, view: View::Log, scroll: follow },
            Case { name: "l closes the log", from: screen(View::Log, 0, 0), key: Key::Char('l'), scrolled: 0, view: View::Main, scroll: 0 },
            Case { name: "l from the help opens the log", from: screen(View::Help, 2, 0), key: Key::Char('l'), scrolled: 9, view: View::Log, scroll: follow },
            Case { name: "Esc returns to the main view", from: screen(View::Log, 5, 0), key: Key::Esc, scrolled: 9, view: View::Main, scroll: 0 },
            Case { name: "down scrolls the help one line", from: screen(View::Help, 0, 0), key: Key::Down, scrolled: 9, view: View::Help, scroll: 1 },
            Case { name: "j stops at the end", from: screen(View::Help, 9, 0), key: Key::Char('j'), scrolled: 9, view: View::Help, scroll: 9 },
            Case { name: "up from the log's end leaves it", from: screen(View::Log, follow, 0), key: Key::Up, scrolled: 9, view: View::Log, scroll: 8 },
            Case { name: "down to the log's end follows it again", from: screen(View::Log, 8, 0), key: Key::Down, scrolled: 9, view: View::Log, scroll: follow },
            Case { name: "page down moves a screen less eight lines", from: screen(View::Help, 0, 30), key: Key::PageDown, scrolled: 90, view: View::Help, scroll: 22 },
            Case { name: "page up stops at the top", from: screen(View::Help, 5, 30), key: Key::PageUp, scrolled: 90, view: View::Help, scroll: 0 },
            Case { name: "g goes to the top", from: screen(View::Log, follow, 0), key: Key::Char('g'), scrolled: 9, view: View::Log, scroll: 0 },
            Case { name: "End goes to the help's end", from: screen(View::Help, 0, 0), key: Key::End, scrolled: 9, view: View::Help, scroll: 9 },
            Case { name: "a scroll beyond the content clamps first", from: screen(View::Help, 50, 0), key: Key::Up, scrolled: 9, view: View::Help, scroll: 8 },
            Case { name: "the main view does not scroll", from: screen(View::Main, 0, 0), key: Key::Down, scrolled: 0, view: View::Main, scroll: 0 },
            Case { name: "other keys change nothing", from: screen(View::Help, 2, 0), key: Key::Char('x'), scrolled: 9, view: View::Help, scroll: 2 },
        ];
        for case in cases {
            let (got, quit) = press(case.from, case.key, case.scrolled);
            assert!(!quit, "{}: quit", case.name);
            assert_eq!((got.view, got.scroll), (case.view, case.scroll), "{}", case.name);
            assert_eq!(
                (got.width, got.height, got.keys),
                (case.from.width, case.from.height, true),
                "{}: the terminal's size and the keys survive",
                case.name
            );
        }
    }

    #[test]
    fn quit_keys_quit_and_change_nothing() {
        for key in ['q', 'Q', '\x03', '\x04'] {
            let from = screen(View::Log, 3, 20);
            let (got, quit) = press(from, Key::Char(key), 9);
            assert!(quit, "{key:?}");
            assert_eq!(got, from);
        }
    }

    #[test]
    fn letter_aliases_scroll_like_the_named_keys() {
        let from = screen(View::Help, 10, 30);
        for (alias, named) in [
            ('k', Key::Up),
            ('j', Key::Down),
            ('b', Key::PageUp),
            (' ', Key::PageDown),
            ('f', Key::PageDown),
            ('g', Key::Home),
            ('G', Key::End),
            ('h', Key::Char('?')),
            ('H', Key::Char('?')),
        ] {
            assert_eq!(
                press(from, Key::Char(alias), 90),
                press(from, named, 90),
                "{alias:?}"
            );
        }
    }

    #[test]
    fn log_reaching_its_end_by_page_follows_it() {
        let (got, _) = press(screen(View::Log, 85, 30), Key::PageDown, 90);
        assert_eq!(got.scroll, usize::MAX);
    }

    #[test]
    fn decode_maps_every_case_of_go_test_decode_keys() {
        let c = Key::Char;
        for (input, want) in [
            ("?", vec![c('?')]),
            ("\x1b", vec![Key::Esc]),
            ("\x1b[A\x1b[B", vec![Key::Up, Key::Down]),
            ("\x1bOA", vec![Key::Up]),
            ("\x1b[5~\x1b[6~", vec![Key::PageUp, Key::PageDown]),
            (
                "\x1b[1~\x1b[4~\x1b[H\x1b[F",
                vec![Key::Home, Key::End, Key::Home, Key::End],
            ),
            ("\x1b[15~q", vec![c('q')]),
            ("\x1bq", vec![Key::Esc, c('q')]),
            ("\x1b[", vec![]),
        ] {
            assert_eq!(decode(input.as_bytes()), want, "{input:?}");
        }
    }

    #[test]
    fn every_sequence_decodes_at_every_split_point() {
        for (bytes, want) in [
            ("\x1b[A", Key::Up),
            ("\x1b[B", Key::Down),
            ("\x1bOA", Key::Up),
            ("\x1bOB", Key::Down),
            ("\x1b[5~", Key::PageUp),
            ("\x1b[6~", Key::PageDown),
            ("\x1b[H", Key::Home),
            ("\x1bOF", Key::End),
        ] {
            for split in 1..bytes.len() {
                let mut decoder = Decoder::default();
                assert_eq!(decoder.push(&bytes.as_bytes()[..split]), [], "{bytes:?}/{split}");
                assert!(decoder.is_pending(), "{bytes:?}/{split}");
                assert_eq!(decoder.push(&bytes.as_bytes()[split..]), [want], "{bytes:?}/{split}");
                assert!(!decoder.is_pending());
            }
        }
    }

    #[test]
    fn a_byte_by_byte_arrow_is_one_key() {
        let mut decoder = Decoder::default();
        let keys: Vec<Key> = b"\x1b[Aj"
            .iter()
            .flat_map(|byte| decoder.push(&[*byte]))
            .collect();
        assert_eq!(keys, [Key::Up, Key::Char('j')]);
    }

    #[test]
    fn a_held_prefix_flushes_as_escape_or_nothing() {
        for (prefix, want) in [
            ("\x1b", vec![Key::Esc]),
            ("\x1b[", vec![]),
            ("\x1bO", vec![]),
        ] {
            let mut decoder = Decoder::default();
            assert_eq!(decoder.push(prefix.as_bytes()), []);
            assert_eq!(decoder.flush(), want, "{prefix:?}");
            assert!(!decoder.is_pending());
            assert_eq!(decoder.push(b"j"), [Key::Char('j')], "the key after a flush is not swallowed");
        }
    }

    #[test]
    fn an_unfinished_sequence_cannot_fill_the_buffer() {
        let mut decoder = Decoder::default();
        assert_eq!(decoder.push(b"\x1b["), []);
        assert_eq!(decoder.push(&[b'0'; 200]), []);
        assert!(!decoder.is_pending());
    }
}
