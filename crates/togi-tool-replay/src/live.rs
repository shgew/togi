//! Playing a recording on the terminal.

use std::cell::Cell;
use std::io::IsTerminal as _;
use std::os::fd::AsFd as _;
use std::rc::Rc;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use togi_journal::Event;
use togi_watch::model::Model;
use togi_watch::screen::{Journal, Quit, Screen};
use togi_watch::terminal::{Config, Feed, run_with};

use crate::Error;
use crate::playback::{Playback, Speed};

/// How often the dashboard is drawn again, whatever the frames hold: a frame that stays true for the clock still
/// has to follow the events, which play on whether or not it shows anything new.
const TICK: Duration = Duration::from_millis(250);

/// The events played so far, folded into the model only when the count changes. Each journal plays on to one reading
/// of `elapsed`, the real time since playback started, and leaves the time that reading shows in `now`, which the
/// frame is then drawn at: a frame never shows one reading's events at another's time.
struct Replaying<'a, E> {
    events: &'a [Event],
    playback: Playback<'a>,
    elapsed: E,
    now: Rc<Cell<Timestamp>>,
    model: Model,
    shown: usize,
}

/// A feed playing `events` from the first `start` of them at `speed` by the real time `elapsed` reads, and the clock
/// its frames are drawn at, which only reads the time of the feed's last journal and never plays anything on. Its
/// frames carry `skipped`, the report of what the read of the recording left out, from the first one on.
fn replaying<E: FnMut() -> Duration>(
    events: &[Event],
    start: usize,
    speed: Speed,
    skipped: Option<String>,
    elapsed: E,
) -> (Replaying<'_, E>, impl Fn() -> Timestamp + use<E>) {
    let mut model = Model::new();
    model.set_skipped(skipped);
    let mut playback = Playback::new(events, start, speed);
    let now = Rc::new(Cell::new(playback.advance(Duration::ZERO).1));
    let clock = {
        let now = Rc::clone(&now);
        move || now.get()
    };
    let feed = Replaying {
        events,
        playback,
        elapsed,
        now,
        model,
        shown: 0,
    };
    (feed, clock)
}

impl<E: FnMut() -> Duration> Feed for Replaying<'_, E> {
    fn journal(&mut self, _: &Screen) -> Journal<'_> {
        let (played, now) = self.playback.advance((self.elapsed)());
        self.now.set(now);
        if played != self.shown {
            let batch = &self.events[self.shown..played];
            // A refused batch stays in the model's problem state for the frame below.
            let _ = self.model.fold_batch(batch);
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

/// Plays `events` from the first `start` of them at `speed` until the viewer quits or a signal ends it, with
/// `skipped` in the footer.
pub(crate) fn play(
    events: &[Event],
    start: usize,
    speed: Speed,
    skipped: Option<String>,
) -> Result<(), Error> {
    let stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return Err(Error::NotTerminal);
    }
    let started = Instant::now();
    let (feed, clock) = replaying(events, start, speed, skipped, move || started.elapsed());
    let stdin = std::io::stdin();
    run_with(
        feed,
        &Config {
            day_controls: false,
            watch: None,
            tick: Some(TICK),
            stop: None,
            terminating: true,
            quit: Quit::Keys,
            switch: None,
        },
        clock,
        stdout.as_fd(),
        Some(stdin.as_fd()),
    )
    .map(drop)
    .map_err(Error::Watch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use togi_machine::BootId;
    use togi_watchtest::Scenario;

    type Elapsed = Rc<Cell<Duration>>;

    /// A feed over `events` whose real time is the returned cell, set by the test and never by the wall clock, and
    /// the clock its frames are drawn at.
    fn at(
        events: &[Event],
        start: usize,
        speed: f64,
        already: Duration,
    ) -> (
        Replaying<'_, impl FnMut() -> Duration>,
        impl Fn() -> Timestamp,
        Elapsed,
    ) {
        let elapsed = Elapsed::new(Cell::new(already));
        let reads = Rc::clone(&elapsed);
        let (feed, clock) = replaying(events, start, Speed::new(speed).unwrap(), None, move || {
            reads.get()
        });
        (feed, clock, elapsed)
    }

    fn screen() -> Screen {
        Screen {
            day_controls: false,
            view: togi_watch::screen::View::Log,
            keys: true,
            scroll: usize::MAX,
            width: 80,
            height: 24,
            one_frame: false,
            colours: togi_watch::screen::Colours::None,
            quit: Quit::Keys,
            switch: togi_watch::screen::Switch::Off,
        }
    }

    fn logged(feed: &mut Replaying<'_, impl FnMut() -> Duration>) -> usize {
        let Journal::Ready { log, .. } = feed.journal(&screen()) else {
            panic!("not ready")
        };
        log.total
    }

    #[test]
    fn the_feed_folds_the_events_played_so_far_and_only_when_the_count_changes() {
        let mut events = Scenario::Controls.events().unwrap();
        events.truncate(8);
        for (hour, event) in (0_i64..).zip(&mut events) {
            event.mono = None;
            event.time = jiff::Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(hour);
        }
        let (mut feed, _, _) = at(&events, 3, 1.0, Duration::ZERO);
        let first = logged(&mut feed);
        assert_eq!(feed.shown, 3);
        assert_eq!(logged(&mut feed), first);
        let (mut all, _, _) = at(&events, 3, 1e9, Duration::from_secs(1));
        logged(&mut all);
        assert_eq!(all.shown, events.len());
        assert!(!all.reload());
    }

    #[test]
    fn what_the_read_skipped_is_in_every_frame_before_and_while_the_events_play() {
        let report = "skipped 1 event of a kind this build does not know (future.thing at line 3)";
        let mut events = Scenario::Controls.events().unwrap();
        events.truncate(8);
        for (hour, event) in (0_i64..).zip(&mut events) {
            event.mono = None;
            event.time = jiff::Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(hour);
        }
        let elapsed = Elapsed::new(Cell::new(Duration::ZERO));
        let reads = Rc::clone(&elapsed);
        let (mut feed, _) = replaying(
            &events,
            1,
            Speed::new(1e9).unwrap(),
            Some(report.to_owned()),
            move || reads.get(),
        );
        for seconds in [0, 1] {
            elapsed.set(Duration::from_secs(seconds));
            let Journal::Ready { snapshot, .. } = feed.journal(&screen()) else {
                panic!("not ready")
            };
            assert_eq!(snapshot.skipped, Some(report));
        }
        assert_eq!(feed.shown, events.len());
    }

    #[test]
    fn an_event_the_model_cannot_fold_is_shown_as_a_problem_and_nothing_folds_after_it() {
        let mut events = Scenario::Controls.events().unwrap();
        events.truncate(3);
        events[1] = events[0].clone();
        let (mut feed, _, _) = at(&events, 3, 1.0, Duration::ZERO);
        assert!(matches!(feed.journal(&screen()), Journal::Problem(_)));
        assert!(matches!(feed.journal(&screen()), Journal::Problem(_)));
    }

    #[test]
    fn a_frame_is_drawn_at_the_time_of_its_own_journal_even_when_an_event_falls_due_between_them() {
        let hour = jiff::SignedDuration::from_hours(1);
        let mut events = Scenario::Controls.events().unwrap();
        events.truncate(4);
        for (second, event) in (0_i64..).zip(&mut events) {
            event.boot = BootId::from("boot".to_owned());
            event.mono = Some(Duration::from_secs(second.cast_unsigned()));
            event.time = Timestamp::UNIX_EPOCH
                + jiff::SignedDuration::from_secs(second)
                + if second >= 1 {
                    hour
                } else {
                    jiff::SignedDuration::ZERO
                };
        }
        let (mut feed, clock, elapsed) = at(&events, 1, 1.0, Duration::ZERO);
        feed.journal(&screen());
        assert_eq!(feed.shown, 1);
        let before = clock();
        assert_eq!(before, events[0].time);

        // The second event, which a wall-clock correction puts an hour on, falls due after the journal was sampled.
        elapsed.set(Duration::from_secs(1));
        assert_eq!(clock(), before);
        assert_eq!(clock(), before);
        assert_eq!(feed.shown, 1);

        // Only the next journal plays it, and draws its frame at the time it shows.
        feed.journal(&screen());
        assert_eq!(feed.shown, 2);
        assert_eq!(clock(), events[1].time);
    }
}
