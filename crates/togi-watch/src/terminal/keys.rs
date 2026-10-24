//! Keys: decoding the raw bytes a terminal sends, and what a key does to the screen.

use crate::screen::{Direction, Quit, Screen, Switch, View};

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
    /// F10, the key of the night switch.
    F10,
}

/// The most bytes an unfinished sequence may hold before it is dropped.
const PENDING_LIMIT: usize = 128;

/// The key an escape sequence names, from the bytes after the escape: the ones the Linux console and common
/// terminals send for the keys that scroll and for F10.
fn sequence_key(sequence: &[u8]) -> Option<Key> {
    match sequence {
        b"[A" | b"OA" => Some(Key::Up),
        b"[B" | b"OB" => Some(Key::Down),
        b"[5~" => Some(Key::PageUp),
        b"[6~" => Some(Key::PageDown),
        b"[H" | b"OH" | b"[1~" | b"[7~" => Some(Key::Home),
        b"[F" | b"OF" | b"[4~" | b"[8~" => Some(Key::End),
        b"[21~" => Some(Key::F10),
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
        Key::Char(_) | Key::F10 => (screen, false),
    }
}

/// What a key does to the night switch before [`press`] sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// The switch has no use for the key.
    Pass,
    /// The key changed the question on the screen and is used up.
    Screen(Screen),
    /// The viewer answered yes: the dashboard ends and the caller switches.
    Confirmed(Direction),
}

/// Applies a key to the night switch: F10, and `n` where the switch goes into the night, asks the question; then
/// `y` or `Y` confirms it and any other key, Esc and Ctrl-C included, withdraws it and does nothing else.
pub(super) fn switch(screen: Screen, key: Key) -> Step {
    match screen.switch {
        Switch::Off => Step::Pass,
        Switch::Asking(direction) => {
            if matches!(key, Key::Char('y' | 'Y')) {
                Step::Confirmed(direction)
            } else {
                Step::Screen(Screen {
                    switch: Switch::Offered(direction),
                    ..screen
                })
            }
        }
        Switch::Offered(direction) => {
            let night_key = direction == Direction::Night && matches!(key, Key::Char('n' | 'N'));
            if key == Key::F10 || night_key {
                Step::Screen(Screen {
                    switch: Switch::Asking(direction),
                    ..screen
                })
            } else {
                Step::Pass
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Decoder, Key, Step, decode, press, switch};
    use crate::screen::{Colours, Direction, Quit, Screen, Switch, View};
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
            switch: Switch::Off,
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

    #[test_case::test_case('?', View::Help, 0; "question mark opens the help top")]
    #[test_case::test_case('?', View::Help, 6; "question mark opens the help back")]
    #[test_case::test_case('?', View::Help, FOLLOW; "question mark opens the help follow")]
    #[test_case::test_case('h', View::Help, 0; "h opens the help top")]
    #[test_case::test_case('h', View::Help, 6; "h opens the help back")]
    #[test_case::test_case('h', View::Help, FOLLOW; "h opens the help follow")]
    #[test_case::test_case('H', View::Help, 0; "capital h opens the help top")]
    #[test_case::test_case('H', View::Help, 6; "capital h opens the help back")]
    #[test_case::test_case('H', View::Help, FOLLOW; "capital h opens the help follow")]
    #[test_case::test_case('l', View::Log, 0; "l opens the log top")]
    #[test_case::test_case('l', View::Log, 6; "l opens the log back")]
    #[test_case::test_case('l', View::Log, FOLLOW; "l opens the log follow")]
    #[test_case::test_case('L', View::Log, 0; "capital l opens the log top")]
    #[test_case::test_case('L', View::Log, 6; "capital l opens the log back")]
    #[test_case::test_case('L', View::Log, FOLLOW; "capital l opens the log follow")]
    fn a_shortcut_opens_its_view_and_returns_to_main_on_its_own_view(
        key: char,
        target: View,
        scroll: usize,
    ) {
        for view in VIEWS {
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

    #[test_case::test_case(Key::Esc, 0; "escape top")]
    #[test_case::test_case(Key::Esc, 6; "escape back")]
    #[test_case::test_case(Key::Esc, FOLLOW; "escape follow")]
    fn escape_returns_to_the_dashboard_at_its_top(key: Key, scroll: usize) {
        for view in VIEWS {
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

    const EXTENT: usize = 400;

    fn on_every_view(
        key: Key,
        width: u16,
        height: u16,
        check: impl Fn(&dyn Fn(usize) -> usize, usize, usize, &str),
    ) {
        for view in [View::Help, View::Log] {
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

    #[test_case::test_case(Key::Down, 80, 24; "down 80x24")]
    #[test_case::test_case(Key::Down, 160, 45; "down 160x45")]
    #[test_case::test_case(Key::Down, 240, 67; "down 240x67")]
    #[test_case::test_case(Key::Char('j'), 80, 24; "j 80x24")]
    #[test_case::test_case(Key::Char('j'), 160, 45; "j 160x45")]
    #[test_case::test_case(Key::Char('j'), 240, 67; "j 240x67")]
    fn a_line_down_key_scrolls_one_line_to_the_end(key: Key, width: u16, height: u16) {
        on_every_view(key, width, height, |after, _, last, case| {
            assert_eq!(after(10), 11, "{case}");
            assert_eq!(after(EXTENT - 1), last, "{case} end");
        });
    }

    #[test_case::test_case(Key::Up, 80, 24; "up 80x24")]
    #[test_case::test_case(Key::Up, 160, 45; "up 160x45")]
    #[test_case::test_case(Key::Up, 240, 67; "up 240x67")]
    #[test_case::test_case(Key::Char('k'), 80, 24; "k 80x24")]
    #[test_case::test_case(Key::Char('k'), 160, 45; "k 160x45")]
    #[test_case::test_case(Key::Char('k'), 240, 67; "k 240x67")]
    fn a_line_up_key_scrolls_one_line_to_the_top(key: Key, width: u16, height: u16) {
        on_every_view(key, width, height, |after, _, _, case| {
            assert_eq!(after(10), 9, "{case}");
            assert_eq!(after(0), 0, "{case} top");
        });
    }

    #[test_case::test_case(Key::PageDown, 80, 24; "page down 80x24")]
    #[test_case::test_case(Key::PageDown, 160, 45; "page down 160x45")]
    #[test_case::test_case(Key::PageDown, 240, 67; "page down 240x67")]
    #[test_case::test_case(Key::Char(' '), 80, 24; "space 80x24")]
    #[test_case::test_case(Key::Char(' '), 160, 45; "space 160x45")]
    #[test_case::test_case(Key::Char(' '), 240, 67; "space 240x67")]
    #[test_case::test_case(Key::Char('f'), 80, 24; "f 80x24")]
    #[test_case::test_case(Key::Char('f'), 160, 45; "f 160x45")]
    #[test_case::test_case(Key::Char('f'), 240, 67; "f 240x67")]
    fn a_page_down_key_scrolls_one_page_to_the_end(key: Key, width: u16, height: u16) {
        on_every_view(key, width, height, |after, page, last, case| {
            assert_eq!(after(10), 10 + page, "{case}");
            assert_eq!(after(EXTENT - 1), last, "{case} end");
        });
    }

    #[test_case::test_case(Key::PageUp, 80, 24; "page up 80x24")]
    #[test_case::test_case(Key::PageUp, 160, 45; "page up 160x45")]
    #[test_case::test_case(Key::PageUp, 240, 67; "page up 240x67")]
    #[test_case::test_case(Key::Char('b'), 80, 24; "b 80x24")]
    #[test_case::test_case(Key::Char('b'), 160, 45; "b 160x45")]
    #[test_case::test_case(Key::Char('b'), 240, 67; "b 240x67")]
    fn a_page_up_key_scrolls_one_page_to_the_top(key: Key, width: u16, height: u16) {
        on_every_view(key, width, height, |after, page, _, case| {
            assert_eq!(after(100), 100 - page, "{case}");
            assert_eq!(after(1), 0, "{case} top");
        });
    }

    #[test_case::test_case(Key::Home, 80, 24; "home 80x24")]
    #[test_case::test_case(Key::Home, 160, 45; "home 160x45")]
    #[test_case::test_case(Key::Home, 240, 67; "home 240x67")]
    #[test_case::test_case(Key::Char('g'), 80, 24; "g 80x24")]
    #[test_case::test_case(Key::Char('g'), 160, 45; "g 160x45")]
    #[test_case::test_case(Key::Char('g'), 240, 67; "g 240x67")]
    fn a_home_key_goes_to_the_top(key: Key, width: u16, height: u16) {
        on_every_view(key, width, height, |after, _, _, case| {
            assert_eq!(after(77), 0, "{case}");
            assert_eq!(after(FOLLOW), 0, "{case} from the end");
        });
    }

    #[test_case::test_case(Key::End, 80, 24; "end 80x24")]
    #[test_case::test_case(Key::End, 160, 45; "end 160x45")]
    #[test_case::test_case(Key::End, 240, 67; "end 240x67")]
    #[test_case::test_case(Key::Char('G'), 80, 24; "capital g 80x24")]
    #[test_case::test_case(Key::Char('G'), 160, 45; "capital g 160x45")]
    #[test_case::test_case(Key::Char('G'), 240, 67; "capital g 240x67")]
    fn an_end_key_goes_to_the_end(key: Key, width: u16, height: u16) {
        on_every_view(key, width, height, |after, _, last, case| {
            assert_eq!(after(3), last, "{case}");
        });
    }

    #[test_case::test_case(0; "height 0")]
    #[test_case::test_case(1; "height 1")]
    #[test_case::test_case(2; "height 2")]
    #[test_case::test_case(3; "height 3")]
    #[test_case::test_case(12; "height 12")]
    #[test_case::test_case(24; "height 24")]
    #[test_case::test_case(29; "height 29")]
    #[test_case::test_case(30; "height 30")]
    #[test_case::test_case(45; "height 45")]
    #[test_case::test_case(67; "height 67")]
    fn a_page_key_moves_a_screen_less_eight_lines_and_at_least_one_line(height: u16) {
        for view in [View::Help, View::Log] {
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
        let tiny = screen(View::Log, 0, 1);
        assert_eq!(
            press(tiny, Key::PageDown, 50).0.scroll,
            1,
            "the page is at least one line"
        );
    }

    #[test_case::test_case('+', 0; "plus top")]
    #[test_case::test_case('+', 4; "plus back")]
    #[test_case::test_case('+', FOLLOW; "plus follow")]
    #[test_case::test_case('-', 0; "minus top")]
    #[test_case::test_case('-', 4; "minus back")]
    #[test_case::test_case('-', FOLLOW; "minus follow")]
    #[test_case::test_case('=', 0; "equals top")]
    #[test_case::test_case('=', 4; "equals back")]
    #[test_case::test_case('=', FOLLOW; "equals follow")]
    #[test_case::test_case('_', 0; "underscore top")]
    #[test_case::test_case('_', 4; "underscore back")]
    #[test_case::test_case('_', FOLLOW; "underscore follow")]
    fn zoom_keys_do_nothing_on_every_view(key: char, scroll: usize) {
        for view in VIEWS {
            let from = screen(view, scroll, 30);
            assert_eq!(
                press(from, Key::Char(key), 90),
                (from, false),
                "{key:?} on {view:?}"
            );
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

    #[test_case::test_case('q', 0; "q top")]
    #[test_case::test_case('q', 3; "q back")]
    #[test_case::test_case('q', FOLLOW; "q follow")]
    #[test_case::test_case('Q', 0; "capital q top")]
    #[test_case::test_case('Q', 3; "capital q back")]
    #[test_case::test_case('Q', FOLLOW; "capital q follow")]
    #[test_case::test_case('\x03', 0; "ctrl c top")]
    #[test_case::test_case('\x03', 3; "ctrl c back")]
    #[test_case::test_case('\x03', FOLLOW; "ctrl c follow")]
    #[test_case::test_case('\x04', 0; "ctrl d top")]
    #[test_case::test_case('\x04', 3; "ctrl d back")]
    #[test_case::test_case('\x04', FOLLOW; "ctrl d follow")]
    fn the_tuning_boot_ignores_a_quit_key_on_every_view_and_scroll(key: char, scroll: usize) {
        for view in VIEWS {
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
    #[test_case("\x1b[21~", &[Key::F10]; "f10")]
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
    fn offering(switch: Switch) -> Screen {
        Screen {
            switch,
            ..screen(View::Main, 0, 0)
        }
    }

    #[test_case(Direction::Day, Key::F10; "f10 into the day")]
    #[test_case(Direction::Night, Key::F10; "f10 into the night")]
    #[test_case(Direction::Night, Key::Char('n'); "n into the night")]
    #[test_case(Direction::Night, Key::Char('N'); "capital n into the night")]
    fn the_key_of_an_offered_switch_asks_before_it_does_anything(direction: Direction, key: Key) {
        let offered = offering(Switch::Offered(direction));
        assert_eq!(
            switch(offered, key),
            Step::Screen(offering(Switch::Asking(direction)))
        );
    }

    #[test_case(Direction::Day, Key::Char('n'); "n does not leave the night")]
    #[test_case(Direction::Day, Key::Char('y'); "y without a question")]
    #[test_case(Direction::Night, Key::Char('y'); "y without a question before the night")]
    #[test_case(Direction::Night, Key::Esc; "esc")]
    #[test_case(Direction::Day, Key::Char('q'); "q")]
    fn other_keys_pass_an_offered_switch_by(direction: Direction, key: Key) {
        assert_eq!(
            switch(offering(Switch::Offered(direction)), key),
            Step::Pass
        );
    }

    #[test]
    fn a_dashboard_without_a_switch_passes_every_key_by() {
        for key in [Key::F10, Key::Char('n'), Key::Char('y')] {
            assert_eq!(switch(offering(Switch::Off), key), Step::Pass);
        }
    }

    #[test_case(Key::Char('y'); "y")]
    #[test_case(Key::Char('Y'); "capital y")]
    fn yes_confirms_the_question_in_its_direction(key: Key) {
        for direction in [Direction::Night, Direction::Day] {
            assert_eq!(
                switch(offering(Switch::Asking(direction)), key),
                Step::Confirmed(direction)
            );
        }
    }

    #[test_case(Key::Char('n'); "n")]
    #[test_case(Key::Char('N'); "capital n")]
    #[test_case(Key::Esc; "esc")]
    #[test_case(Key::F10; "f10 again")]
    #[test_case(Key::Char('q'); "q")]
    #[test_case(Key::Char('\x03'); "ctrl c")]
    #[test_case(Key::Char(' '); "space")]
    #[test_case(Key::Up; "up")]
    fn any_other_key_withdraws_the_question_and_changes_nothing_else(key: Key) {
        for direction in [Direction::Night, Direction::Day] {
            assert_eq!(
                switch(offering(Switch::Asking(direction)), key),
                Step::Screen(offering(Switch::Offered(direction)))
            );
        }
    }
}
