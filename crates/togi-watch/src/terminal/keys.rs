//! Keys: decoding the raw bytes a terminal sends, and what a key does to the screen.

use crate::frames::body_rows;
use crate::screen::{Quit, Screen, View};

/// One key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Key {
    /// A printable or control character (a byte of other input maps to the character of the same value).
    Char(char),
    Esc,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
}

/// The most bytes an unfinished sequence may hold before it is dropped.
const PENDING_LIMIT: usize = 128;

/// The key an escape sequence names, from the bytes after the escape: the ones the Linux console and common
/// terminals send for the arrows, page keys, Home and End, in both the CSI (`[`) and SS3 (`O`) forms.
fn sequence_key(sequence: &[u8]) -> Option<Key> {
    match sequence {
        b"[A" | b"OA" => Some(Key::Up),
        b"[B" | b"OB" => Some(Key::Down),
        b"[C" | b"OC" => Some(Key::Right),
        b"[D" | b"OD" => Some(Key::Left),
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

/// The main page, unless it is already shown: Esc and `m` do nothing there.
fn main_view(screen: Screen) -> Screen {
    if screen.view == View::Main {
        screen
    } else {
        view(screen, View::Main)
    }
}

/// `to` as a fresh page: detail pages open at their top, the journal at its end, where it follows new events.
fn view(screen: Screen, view: View) -> Screen {
    Screen {
        view,
        scroll: if view == View::Log { usize::MAX } else { 0 },
        ..screen
    }
}

/// Applies a key to the screen and reports whether to quit.
///
/// Left and Right walk the nine pages with wrap; `?` and `h` open the help, `l` the journal, and pressing a
/// page's own shortcut on it returns to the main page, as do Esc and `m`. The up and down arrows (or `k`, `j`)
/// scroll a line, the page keys (or `b`, space, `f`) a body height less one line, Home and End (or `g`, `G`) go
/// to the ends of a page that can scroll up to `scrolled` lines. A page change resets the scroll: the pages open
/// at their top and the journal at its end (`usize::MAX`), where it keeps following new events until a scroll
/// leaves the end; a key that scrolls never changes the page, and nothing but a key changes the scroll, so a
/// background refresh never resets it. `q`, `Q`, Ctrl-C and Ctrl-D quit unless the screen's [`Quit`] is
/// `Ignored`, which makes them do nothing on every page. Every other key, `+` and `-` among them, does nothing.
pub(super) fn press(screen: Screen, key: Key, scrolled: usize) -> (Screen, bool) {
    let page = body_rows(&screen).saturating_sub(1).max(1);
    let at = screen.scroll.min(scrolled);
    let to = |line: usize| {
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
        Key::Left => (view(screen, screen.view.prev()), false),
        Key::Right => (view(screen, screen.view.next()), false),
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
        Key::Esc | Key::Char('m') => (main_view(screen), false),
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
    use crate::frames::body_rows;
    use crate::screen::{Colours, Quit, Screen, View};
    use test_case::test_case;

    const FOLLOW: usize = usize::MAX;

    /// Where a page opens: its top, or for the journal its end, which it follows.
    fn opening(view: View) -> usize {
        if view == View::Log { FOLLOW } else { 0 }
    }

    /// How far a page key moves on `from`.
    fn page(from: &Screen) -> usize {
        body_rows(from).saturating_sub(1).max(1)
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
    #[test_case(screen(View::Cores, 5, 0), Key::Char('m'), 9, View::Main, 0; "m returns to the main view")]
    #[test_case(screen(View::Main, 6, 0), Key::Esc, 20, View::Main, 6; "esc on the main view leaves its scroll alone")]
    #[test_case(screen(View::Help, 0, 0), Key::Down, 9, View::Help, 1; "down scrolls the help one line")]
    #[test_case(screen(View::Help, 9, 0), Key::Char('j'), 9, View::Help, 9; "j stops at the end")]
    #[test_case(screen(View::Log, FOLLOW, 0), Key::Up, 9, View::Log, 8; "up from the log's end leaves it")]
    #[test_case(screen(View::Log, 8, 0), Key::Down, 9, View::Log, FOLLOW; "down to the log's end follows it again")]
    #[test_case(screen(View::Help, 5, 30), Key::PageUp, 90, View::Help, 0; "page up stops at the top")]
    #[test_case(screen(View::Log, FOLLOW, 0), Key::Char('g'), 9, View::Log, 0; "g goes to the top")]
    #[test_case(screen(View::Help, 0, 0), Key::End, 9, View::Help, 9; "end goes to the help's end")]
    #[test_case(screen(View::Help, 50, 0), Key::Up, 9, View::Help, 8; "a scroll beyond the content clamps first")]
    #[test_case(screen(View::Main, 0, 0), Key::Down, 0, View::Main, 0; "a main view that fits does not scroll")]
    #[test_case(screen(View::Main, 3, 0), Key::Down, 20, View::Main, 4; "a main view that does not fit scrolls by the extent the frame reports")]
    #[test_case(screen(View::Main, 20, 0), Key::End, 20, View::Main, 20; "the compact main view stops at its end")]
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

    #[test_case(Key::Right, 1; "right walks to the next page")]
    #[test_case(Key::Left, 8; "left walks to the previous page")]
    fn an_arrow_walks_the_nine_pages_with_wrap_from_every_page(key: Key, step: usize) {
        assert_eq!(View::ALL.len(), 9);
        for (at, view) in View::ALL.into_iter().enumerate() {
            let want = View::ALL[(at + step) % 9];
            for scroll in [0, 7, FOLLOW] {
                let from = screen(view, scroll, 24);
                let (got, quit) = press(from, key, 40);
                assert!(!quit);
                assert_eq!(got.view, want, "{view:?} {key:?}");
                assert_eq!(
                    got.scroll,
                    opening(want),
                    "{view:?} {key:?} from scroll {scroll}: the new page opens where pages open"
                );
                assert_eq!(
                    (got.width, got.height, got.keys, got.quit),
                    (from.width, from.height, true, from.quit)
                );
            }
        }
    }

    #[test_case(Key::Right; "right")]
    #[test_case(Key::Left; "left")]
    fn nine_arrows_visit_every_page_once_and_come_home(key: Key) {
        for start in View::ALL {
            let mut now = screen(start, 0, 24);
            let mut seen = vec![now.view];
            for _ in 0..9 {
                now = press(now, key, 40).0;
                seen.push(now.view);
            }
            assert_eq!(now.view, start, "{key:?} from {start:?}");
            seen.pop();
            seen.sort_by_key(|view| view.number());
            assert_eq!(seen, View::ALL, "{key:?} from {start:?}");
        }
    }

    #[test]
    fn left_from_main_wraps_to_the_last_page_and_right_returns() {
        let main = press(screen(View::Main, 0, 24), Key::Left, 0).0;
        assert_eq!(
            main.view,
            View::Help,
            "Left from Main wraps to the last page"
        );
        assert_eq!(press(main, Key::Right, 0).0.view, View::Main);
    }

    #[test_case('?', View::Help; "question mark opens the help")]
    #[test_case('h', View::Help; "h opens the help")]
    #[test_case('H', View::Help; "capital h opens the help")]
    #[test_case('l', View::Log; "l opens the log")]
    #[test_case('L', View::Log; "capital l opens the log")]
    fn a_shortcut_opens_its_page_and_returns_to_main_on_its_own_page(key: char, target: View) {
        for view in View::ALL {
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
    #[test_case(Key::Char('m'); "m")]
    fn a_return_key_goes_to_main_and_keeps_the_main_scroll(key: Key) {
        for view in View::ALL {
            for scroll in [0, 6, FOLLOW] {
                let from = screen(view, scroll, 24);
                let (got, quit) = press(from, key, 40);
                assert!(!quit);
                let want_scroll = if view == View::Main { from.scroll } else { 0 };
                assert_eq!(
                    (got.view, got.scroll),
                    (View::Main, want_scroll),
                    "{key:?} on {view:?} at {scroll}"
                );
            }
        }
    }

    #[test]
    fn every_scroll_key_scrolls_every_page_and_never_changes_it() {
        let extent = 400;
        for view in View::ALL {
            for (width, height) in [(80, 24), (160, 45), (240, 67)] {
                let base = Screen {
                    width,
                    ..screen(view, 0, height)
                };
                let page = page(&base);
                let at = |scroll| Screen { scroll, ..base };
                let after = |from: Screen, key| {
                    let (got, quit) = press(from, key, extent);
                    assert!(!quit);
                    assert_eq!(got.view, view, "{key:?} changed the page");
                    got.scroll
                };
                let last = if view == View::Log { FOLLOW } else { extent };
                for key in [Key::Down, Key::Char('j')] {
                    assert_eq!(after(at(10), key), 11, "{view:?} {key:?}");
                    assert_eq!(after(at(extent - 1), key), last, "{view:?} {key:?} end");
                }
                for key in [Key::Up, Key::Char('k')] {
                    assert_eq!(after(at(10), key), 9, "{view:?} {key:?}");
                    assert_eq!(after(at(0), key), 0, "{view:?} {key:?} top");
                }
                for key in [Key::PageDown, Key::Char(' '), Key::Char('f')] {
                    assert_eq!(after(at(10), key), 10 + page, "{view:?} {key:?} {height}");
                    assert_eq!(after(at(extent - 1), key), last, "{view:?} {key:?} end");
                }
                for key in [Key::PageUp, Key::Char('b')] {
                    assert_eq!(after(at(100), key), 100 - page, "{view:?} {key:?} {height}");
                    assert_eq!(after(at(1), key), 0, "{view:?} {key:?} top");
                }
                for key in [Key::Home, Key::Char('g')] {
                    assert_eq!(after(at(77), key), 0, "{view:?} {key:?}");
                    assert_eq!(after(at(FOLLOW), key), 0, "{view:?} {key:?} from the end");
                }
                for key in [Key::End, Key::Char('G')] {
                    assert_eq!(after(at(3), key), last, "{view:?} {key:?}");
                }
            }
        }
    }

    #[test]
    fn a_page_key_moves_the_body_height_less_one_and_at_least_one_line() {
        for view in View::ALL {
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
        let tiny = screen(View::Cores, 0, 1);
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
    fn zoom_keys_do_nothing_on_every_page(key: char) {
        for view in View::ALL {
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
    fn a_quit_key_quits_and_changes_nothing_on_every_page(key: char) {
        for view in View::ALL {
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
    fn the_tuning_boot_ignores_a_quit_key_on_every_page_and_scroll(key: char) {
        for view in View::ALL {
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
    #[test_case('m', Key::Esc; "m is escape")]
    fn a_letter_alias_acts_like_the_named_key_on_every_page(alias: char, named: Key) {
        for view in View::ALL {
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
    #[test_case("\x1b[C\x1b[D", &[Key::Right, Key::Left]; "csi right and left")]
    #[test_case("\x1bOC\x1bOD", &[Key::Right, Key::Left]; "ss3 right and left")]
    #[test_case("\x1b[D\x1bOCq", &[Key::Left, Key::Right, Key::Char('q')]; "mixed csi and ss3 arrows then a character")]
    #[test_case("\x1b[5~\x1b[6~", &[Key::PageUp, Key::PageDown]; "page up and page down")]
    #[test_case("\x1b[1~\x1b[4~\x1b[H\x1b[F", &[Key::Home, Key::End, Key::Home, Key::End]; "home and end in every form")]
    #[test_case("\x1b[15~q", &[Key::Char('q')]; "an unmapped tilde sequence is dropped")]
    #[test_case("\x1bq", &[Key::Esc, Key::Char('q')]; "escape then a character")]
    #[test_case("\x1b[", &[]; "an unfinished sequence yields nothing")]
    fn decode_maps_input_bytes_to_keys(input: &str, want: &[Key]) {
        assert_eq!(decode(input.as_bytes()), want);
    }

    #[test_case("\x1b[A", Key::Up; "csi up")]
    #[test_case("\x1b[B", Key::Down; "csi down")]
    #[test_case("\x1b[C", Key::Right; "csi right")]
    #[test_case("\x1b[D", Key::Left; "csi left")]
    #[test_case("\x1bOC", Key::Right; "ss3 right")]
    #[test_case("\x1bOD", Key::Left; "ss3 left")]
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
    #[test_case(b"\x1b[C", Key::Right; "csi right")]
    #[test_case(b"\x1bOC", Key::Right; "ss3 right")]
    #[test_case(b"\x1b[D", Key::Left; "csi left")]
    #[test_case(b"\x1bOD", Key::Left; "ss3 left")]
    fn a_byte_by_byte_arrow_is_one_key(arrow: &[u8], key: Key) {
        let mut decoder = Decoder::default();
        let keys: Vec<Key> = arrow
            .iter()
            .chain(b"j")
            .flat_map(|byte| decoder.push(&[*byte]))
            .collect();
        assert_eq!(keys, [key, Key::Char('j')]);
    }

    #[test]
    fn horizontal_arrows_split_across_reads_stay_whole_keys() {
        let mut decoder = Decoder::default();
        let mut keys = decoder.push(b"\x1b[C\x1b");
        keys.extend(decoder.push(b"[D\x1bO"));
        keys.extend(decoder.push(b"C"));
        assert_eq!(keys, [Key::Right, Key::Left, Key::Right]);
        assert!(!decoder.is_pending());
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
