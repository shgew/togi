//! Keys: decoding the raw bytes a terminal sends, and what a key does to the screen.

use crate::screen::{Quit, Screen, View};

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
/// to `scrolled` lines, and `q`, `Q`, Ctrl-C and Ctrl-D quit unless the screen's [`Quit`] is `Ignored`, which makes
/// them do nothing on every view. The help opens at its top and the log at its end, where it keeps following new
/// events (`usize::MAX`) until a scroll leaves the end. Reports whether to quit.
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
        Key::Char('q' | 'Q' | '\x03' | '\x04') => (screen, screen.quit == Quit::Keys),
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
    use crate::screen::{Colours, Quit, Screen, View};
    use test_case::test_case;

    const FOLLOW: usize = usize::MAX;
    const VIEWS: [View; 3] = [View::Main, View::Help, View::Log];

    /// Where a view opens: its top, or for the journal its end, which it follows.
    fn opening(view: View) -> usize {
        if view == View::Log { FOLLOW } else { 0 }
    }

    /// How far a page key moves on `from`.
    fn page(from: &Screen) -> usize {
        usize::from(from.height.saturating_sub(8)).max(1)
    }

    fn screen(view: View, scroll: usize, height: u16) -> Screen {
        Screen {
            view,
            keys: true,
            scroll,
            width: 80,
            height,
            one_frame: false,
            colours: Colours::None,
            quit: Quit::Keys,
        }
    }

    #[test_case(screen(View::Main, 3, 0), Key::Char('?'), 0, View::Help, 0; "question mark opens the help at its top")]
    #[test_case(screen(View::Help, 4, 0), Key::Char('?'), 9, View::Main, 0; "question mark closes the help")]
    #[test_case(screen(View::Main, 0, 0), Key::Char('L'), 0, View::Log, FOLLOW; "capital l opens the log at its end")]
    #[test_case(screen(View::Log, 0, 0), Key::Char('l'), 0, View::Main, 0; "l closes the log")]
    #[test_case(screen(View::Help, 2, 0), Key::Char('l'), 9, View::Log, FOLLOW; "l from the help opens the log")]
    #[test_case(screen(View::Log, 5, 0), Key::Esc, 9, View::Main, 0; "esc returns to the main view")]
    #[test_case(screen(View::Main, 6, 0), Key::Esc, 20, View::Main, 0; "esc returns to the dashboard at its top")]
    #[test_case(screen(View::Help, 0, 0), Key::Down, 9, View::Help, 1; "down scrolls the help one line")]
    #[test_case(screen(View::Help, 9, 0), Key::Char('j'), 9, View::Help, 9; "j stops at the end")]
    #[test_case(screen(View::Log, FOLLOW, 0), Key::Up, 9, View::Log, 8; "up from the log's end leaves it")]
    #[test_case(screen(View::Log, 8, 0), Key::Down, 9, View::Log, FOLLOW; "down to the log's end follows it again")]
    #[test_case(screen(View::Help, 5, 30), Key::PageUp, 90, View::Help, 0; "page up stops at the top")]
    #[test_case(screen(View::Log, FOLLOW, 0), Key::Char('g'), 9, View::Log, 0; "g goes to the top")]
    #[test_case(screen(View::Help, 0, 0), Key::End, 9, View::Help, 9; "end goes to the help's end")]
    #[test_case(screen(View::Help, 50, 0), Key::Up, 9, View::Help, 8; "a scroll beyond the content clamps first")]
    #[test_case(screen(View::Main, 0, 0), Key::Down, 0, View::Main, 0; "a main view that fits does not scroll")]
    #[test_case(screen(View::Main, 3, 0), Key::Down, 20, View::Main, 3; "the dashboard does not scroll")]
    #[test_case(screen(View::Help, 2, 0), Key::Char('x'), 9, View::Help, 2; "other keys change nothing")]
    fn press_maps_each_key_to_its_view_and_scroll(
        from: Screen,
        key: Key,
        scrolled: usize,
        view: View,
        scroll: usize,
    ) {
        let (got, quit) = press(from, key, scrolled);
        assert!(!quit);
        assert_eq!((got.view, got.scroll), (view, scroll));
        assert_eq!(
            (got.width, got.height, got.keys),
            (from.width, from.height, true),
            "the terminal's size and the keys survive"
        );
    }

    #[test_case('?', View::Help; "question mark opens the help")]
    #[test_case('h', View::Help; "h opens the help")]
    #[test_case('H', View::Help; "capital h opens the help")]
    #[test_case('l', View::Log; "l opens the log")]
    #[test_case('L', View::Log; "capital l opens the log")]
    fn a_shortcut_opens_its_view_and_returns_to_main_on_its_own_view(key: char, target: View) {
        for view in VIEWS {
            for scroll in [0, 6, FOLLOW] {
                let from = screen(view, scroll, 24);
                let (got, quit) = press(from, Key::Char(key), 40);
                assert!(!quit);
                let want = if view == target {
                    (View::Main, 0)
                } else {
                    (target, opening(target))
                };
                assert_eq!(
                    (got.view, got.scroll),
                    want,
                    "{key:?} on {view:?} at {scroll}"
                );
            }
        }
    }

    #[test_case(Key::Esc; "escape")]
    fn escape_returns_to_the_dashboard_at_its_top(key: Key) {
        for view in VIEWS {
            for scroll in [0, 6, FOLLOW] {
                let from = screen(view, scroll, 24);
                let (got, quit) = press(from, key, 40);
                assert!(!quit);
                assert_eq!(
                    (got.view, got.scroll),
                    (View::Main, 0),
                    "{key:?} on {view:?} at {scroll}"
                );
            }
        }
    }

    const EXTENT: usize = 400;

    /// Presses `key` on every view at three sizes, checking that it stays on the view, and passes `check` the scroll
    /// `key` reaches from a given scroll, the page step, the view's end and a label naming the case.
    fn on_every_view(key: Key, check: impl Fn(&dyn Fn(usize) -> usize, usize, usize, &str)) {
        for view in [View::Help, View::Log] {
            for (width, height) in [(80, 24), (160, 45), (240, 67)] {
                let base = Screen {
                    width,
                    ..screen(view, 0, height)
                };
                let after = |scroll| {
                    let (got, quit) = press(Screen { scroll, ..base }, key, EXTENT);
                    assert!(!quit);
                    assert_eq!(got.view, view, "{key:?} changed the view");
                    got.scroll
                };
                let last = if view == View::Log { FOLLOW } else { EXTENT };
                check(
                    &after,
                    page(&base),
                    last,
                    &format!("{view:?} {key:?} {height}"),
                );
            }
        }
    }

    #[test_case(Key::Down; "down")]
    #[test_case(Key::Char('j'); "j")]
    fn a_line_down_key_scrolls_one_line_to_the_end(key: Key) {
        on_every_view(key, |after, _, last, case| {
            assert_eq!(after(10), 11, "{case}");
            assert_eq!(after(EXTENT - 1), last, "{case} end");
        });
    }

    #[test_case(Key::Up; "up")]
    #[test_case(Key::Char('k'); "k")]
    fn a_line_up_key_scrolls_one_line_to_the_top(key: Key) {
        on_every_view(key, |after, _, _, case| {
            assert_eq!(after(10), 9, "{case}");
            assert_eq!(after(0), 0, "{case} top");
        });
    }

    #[test_case(Key::PageDown; "page down")]
    #[test_case(Key::Char(' '); "space")]
    #[test_case(Key::Char('f'); "f")]
    fn a_page_down_key_scrolls_one_page_to_the_end(key: Key) {
        on_every_view(key, |after, page, last, case| {
            assert_eq!(after(10), 10 + page, "{case}");
            assert_eq!(after(EXTENT - 1), last, "{case} end");
        });
    }

    #[test_case(Key::PageUp; "page up")]
    #[test_case(Key::Char('b'); "b")]
    fn a_page_up_key_scrolls_one_page_to_the_top(key: Key) {
        on_every_view(key, |after, page, _, case| {
            assert_eq!(after(100), 100 - page, "{case}");
            assert_eq!(after(1), 0, "{case} top");
        });
    }

    #[test_case(Key::Home; "home")]
    #[test_case(Key::Char('g'); "g")]
    fn a_home_key_goes_to_the_top(key: Key) {
        on_every_view(key, |after, _, _, case| {
            assert_eq!(after(77), 0, "{case}");
            assert_eq!(after(FOLLOW), 0, "{case} from the end");
        });
    }

    #[test_case(Key::End; "end")]
    #[test_case(Key::Char('G'); "capital g")]
    fn an_end_key_goes_to_the_end(key: Key) {
        on_every_view(key, |after, _, last, case| {
            assert_eq!(after(3), last, "{case}");
        });
    }

    #[test]
    fn a_page_key_moves_a_screen_less_eight_lines_and_at_least_one_line() {
        for view in [View::Help, View::Log] {
            for height in [0, 1, 2, 3, 12, 24, 29, 30, 45, 67] {
                let from = screen(view, 5, height);
                let moved = page(&from);
                assert!(moved >= 1, "{view:?} {height}");
                let down = press(from, Key::PageDown, 1000).0.scroll;
                assert_eq!(down, 5 + moved, "{view:?} {height}");
                let far = Screen {
                    scroll: 500,
                    ..from
                };
                let up = press(far, Key::PageUp, 1000).0.scroll;
                assert_eq!(up, 500 - moved, "{view:?} {height}");
            }
        }
        let tiny = screen(View::Log, 0, 1);
        assert_eq!(
            press(tiny, Key::PageDown, 50).0.scroll,
            1,
            "the page is at least one line"
        );
    }

    #[test_case('+'; "plus")]
    #[test_case('-'; "minus")]
    #[test_case('='; "equals")]
    #[test_case('_'; "underscore")]
    fn zoom_keys_do_nothing_on_every_view(key: char) {
        for view in VIEWS {
            for scroll in [0, 4, FOLLOW] {
                let from = screen(view, scroll, 30);
                assert_eq!(
                    press(from, Key::Char(key), 90),
                    (from, false),
                    "{key:?} on {view:?}"
                );
            }
        }
    }

    #[test_case('q'; "q")]
    #[test_case('Q'; "capital q")]
    #[test_case('\x03'; "ctrl c")]
    #[test_case('\x04'; "ctrl d")]
    fn a_quit_key_quits_and_changes_nothing_on_every_view(key: char) {
        for view in VIEWS {
            let from = screen(view, 3, 20);
            let (got, quit) = press(from, Key::Char(key), 9);
            assert!(quit, "{view:?} {key:?}");
            assert_eq!(got, from);
        }
    }

    #[test_case('q'; "q")]
    #[test_case('Q'; "capital q")]
    #[test_case('\x03'; "ctrl c")]
    #[test_case('\x04'; "ctrl d")]
    fn the_tuning_boot_ignores_a_quit_key_on_every_view_and_scroll(key: char) {
        for view in VIEWS {
            for scroll in [0, 3, FOLLOW] {
                let from = Screen {
                    quit: Quit::Ignored,
                    ..screen(view, scroll, 20)
                };
                let (got, quit) = press(from, Key::Char(key), 9);
                assert!(!quit, "{view:?} scroll {scroll} {key:?} quit");
                assert_eq!(
                    got, from,
                    "{view:?} scroll {scroll} {key:?} changed the screen"
                );
            }
        }
    }

    #[test_case('k', Key::Up; "k is up")]
    #[test_case('j', Key::Down; "j is down")]
    #[test_case('b', Key::PageUp; "b is page up")]
    #[test_case(' ', Key::PageDown; "space is page down")]
    #[test_case('f', Key::PageDown; "f is page down")]
    #[test_case('g', Key::Home; "g is home")]
    #[test_case('G', Key::End; "capital g is end")]
    #[test_case('h', Key::Char('?'); "h is the help key")]
    #[test_case('H', Key::Char('?'); "capital h is the help key")]
    #[test_case('L', Key::Char('l'); "capital l is the log key")]
    fn a_letter_alias_acts_like_the_named_key_on_every_view(alias: char, named: Key) {
        for view in VIEWS {
            let from = screen(view, 10, 30);
            assert_eq!(
                press(from, Key::Char(alias), 90),
                press(from, named, 90),
                "{alias:?} on {view:?}"
            );
        }
    }

    #[test]
    fn log_reaching_its_end_by_page_follows_it() {
        let from = screen(View::Log, 0, 30);
        let from = Screen {
            scroll: 90 - page(&from),
            ..from
        };
        assert_eq!(press(from, Key::PageDown, 90).0.scroll, FOLLOW);
    }

    #[test_case("?", &[Key::Char('?')]; "a printable character")]
    #[test_case("\x1b", &[Key::Esc]; "a lone escape")]
    #[test_case("\x1b[A\x1b[B", &[Key::Up, Key::Down]; "csi up and down")]
    #[test_case("\x1bOA", &[Key::Up]; "ss3 up")]
    #[test_case("\x1b[5~\x1b[6~", &[Key::PageUp, Key::PageDown]; "page up and page down")]
    #[test_case("\x1b[1~\x1b[4~\x1b[H\x1b[F", &[Key::Home, Key::End, Key::Home, Key::End]; "home and end in every form")]
    #[test_case("\x1b[15~q", &[Key::Char('q')]; "an unmapped tilde sequence is dropped")]
    #[test_case("\x1bq", &[Key::Esc, Key::Char('q')]; "escape then a character")]
    #[test_case("\x1b[", &[]; "an unfinished sequence yields nothing")]
    #[test_case("\x1b[C\x1b[D", &[]; "horizontal csi arrows are ignored")]
    #[test_case("\x1bOC\x1bOD", &[]; "horizontal ss3 arrows are ignored")]
    fn decode_maps_input_bytes_to_keys(input: &str, want: &[Key]) {
        assert_eq!(decode(input.as_bytes()), want);
    }

    #[test_case("\x1b[A", Key::Up; "csi up")]
    #[test_case("\x1b[B", Key::Down; "csi down")]
    #[test_case("\x1bOA", Key::Up; "ss3 up")]
    #[test_case("\x1bOB", Key::Down; "ss3 down")]
    #[test_case("\x1b[5~", Key::PageUp; "page up")]
    #[test_case("\x1b[6~", Key::PageDown; "page down")]
    #[test_case("\x1b[H", Key::Home; "csi home")]
    #[test_case("\x1bOF", Key::End; "ss3 end")]
    fn a_sequence_decodes_at_every_split_point(bytes: &str, want: Key) {
        for split in 1..bytes.len() {
            let mut decoder = Decoder::default();
            assert_eq!(
                decoder.push(&bytes.as_bytes()[..split]),
                [],
                "{bytes:?}/{split}"
            );
            assert!(decoder.is_pending(), "{bytes:?}/{split}");
            assert_eq!(
                decoder.push(&bytes.as_bytes()[split..]),
                [want],
                "{bytes:?}/{split}"
            );
            assert!(!decoder.is_pending());
        }
    }

    #[test_case(b"\x1b[A", Key::Up; "csi up")]
    #[test_case(b"\x1bOA", Key::Up; "ss3 up")]
    fn a_byte_by_byte_arrow_is_one_key(arrow: &[u8], key: Key) {
        let mut decoder = Decoder::default();
        let keys: Vec<Key> = arrow
            .iter()
            .chain(b"j")
            .flat_map(|byte| decoder.push(&[*byte]))
            .collect();
        assert_eq!(keys, [key, Key::Char('j')]);
    }

    #[test_case("\x1b", &[Key::Esc]; "a lone escape flushes as escape")]
    #[test_case("\x1b[", &[]; "a csi prefix flushes as nothing")]
    #[test_case("\x1bO", &[]; "an ss3 prefix flushes as nothing")]
    fn a_held_prefix_flushes_as_escape_or_nothing(prefix: &str, want: &[Key]) {
        let mut decoder = Decoder::default();
        assert_eq!(decoder.push(prefix.as_bytes()), []);
        assert_eq!(decoder.flush(), want);
        assert!(!decoder.is_pending());
        assert_eq!(
            decoder.push(b"j"),
            [Key::Char('j')],
            "the key after a flush is not swallowed"
        );
    }

    #[test]
    fn an_unfinished_sequence_cannot_fill_the_buffer() {
        let mut decoder = Decoder::default();
        assert_eq!(decoder.push(b"\x1b["), []);
        assert_eq!(decoder.push(&[b'0'; 200]), []);
        assert!(!decoder.is_pending());
    }
}
