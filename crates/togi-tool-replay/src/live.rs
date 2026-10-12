//! Playing a recording on the terminal.

use std::cell::RefCell;
use std::io::IsTerminal as _;
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::time::{Duration, Instant};

use togi_journal::Event;
use togi_watch::model::Model;
use togi_watch::screen::{Journal, Screen};
use togi_watch::terminal::{Config, Feed, run_with};

use crate::Error;
use crate::playback::{Playback, Speed};

/// How often the dashboard is drawn again, whatever the frames hold: a frame that stays true for the clock still
/// has to follow the events, which play on whether or not it shows anything new.
const TICK: Duration = Duration::from_millis(250);

/// The events played so far, folded into the model only when the count changes.
struct Replaying<'a> {
    events: &'a [Event],
    playback: Rc<RefCell<Playback<'a>>>,
    started: Instant,
    model: Model,
    shown: usize,
}

impl Feed for Replaying<'_> {
    fn journal(&mut self, _: &Screen) -> Journal<'_> {
        let (played, _) = self.playback.borrow_mut().advance(self.started.elapsed());
        if played != self.shown {
            for event in &self.events[self.shown..played] {
                if self.model.problem().is_some() || self.model.fold(event).is_err() {
                    break;
                }
            }
            self.shown = played;
        }
        if let Some(problem) = self.model.problem() {
            Journal::Problem(problem)
        } else {
            let snapshot = self.model.snapshot();
            Journal::Ready {
                snapshot,
                log: snapshot.log(),
            }
        }
    }

    fn reload(&mut self) -> bool {
        false
    }
}

/// Plays `events` from the first `start` of them at `speed` until the viewer quits or a signal ends it.
pub(crate) fn play(events: &[Event], start: usize, speed: Speed) -> Result<(), Error> {
    let stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return Err(Error::NotTerminal);
    }
    let started = Instant::now();
    let playback = Rc::new(RefCell::new(Playback::new(events, start, speed)));
    let clock = {
        let playback = Rc::clone(&playback);
        move || playback.borrow_mut().advance(started.elapsed()).1
    };
    let feed = Replaying {
        events,
        playback,
        started,
        model: Model::new(),
        shown: 0,
    };
    let stdin = std::io::stdin();
    run_with(
        feed,
        &Config {
            watch: None,
            tick: Some(TICK),
            stop: None,
            terminating: true,
        },
        clock,
        stdout.as_fd(),
        Some(stdin.as_fd()),
    )
    .map_err(Error::Watch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use togi_watchtest::Scenario;

    fn replaying(events: &[Event], start: usize, speed: f64, already: Duration) -> Replaying<'_> {
        let started = Instant::now().checked_sub(already).unwrap();
        Replaying {
            events,
            playback: Rc::new(RefCell::new(Playback::new(
                events,
                start,
                Speed::new(speed).unwrap(),
            ))),
            started,
            model: Model::new(),
            shown: 0,
        }
    }

    fn screen() -> Screen {
        Screen {
            view: togi_watch::screen::View::Log,
            keys: true,
            scroll: usize::MAX,
            width: 80,
            height: 24,
            one_frame: false,
            colours: togi_watch::screen::Colours::None,
        }
    }

    fn logged(feed: &mut Replaying<'_>) -> usize {
        let Journal::Ready { log, .. } = feed.journal(&screen()) else {
            panic!("not ready")
        };
        log.total
    }

    #[test]
    fn the_feed_folds_the_events_played_so_far_and_only_when_the_count_changes() {
        let mut events = Scenario::Checking.events().unwrap();
        events.truncate(8);
        for (hour, event) in (0_i64..).zip(&mut events) {
            event.mono = None;
            event.time = jiff::Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(hour);
        }
        let mut feed = replaying(&events, 3, 1.0, Duration::ZERO);
        let first = logged(&mut feed);
        assert_eq!(feed.shown, 3);
        assert_eq!(logged(&mut feed), first);
        let mut all = replaying(&events, 3, 1e9, Duration::from_secs(1));
        logged(&mut all);
        assert_eq!(all.shown, events.len());
        assert!(!all.reload());
    }

    #[test]
    fn an_event_the_model_cannot_fold_is_shown_as_a_problem_and_nothing_folds_after_it() {
        let mut events = Scenario::Checking.events().unwrap();
        events.truncate(3);
        events[1] = events[0].clone();
        let mut feed = replaying(&events, 3, 1.0, Duration::ZERO);
        assert!(matches!(feed.journal(&screen()), Journal::Problem(_)));
        assert!(matches!(feed.journal(&screen()), Journal::Problem(_)));
    }
}
