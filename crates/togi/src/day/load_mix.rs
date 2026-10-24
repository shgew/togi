//! Sorts sampled per-CPU utilisation into the load shapes the regimes test, so the day service can say how much of
//! daily time resembles each ([Load mix](../../../../docs/spec/runtime.md#load-mix)).

use std::collections::HashMap;
use std::time::Duration;

use togi_journal::LoadMix;
use togi_machine::{CoreInfo, CpuId, CpuTime};

use super::Error;

/// How often per-CPU times are read. A few seconds is long enough for a share of time to mean something and short
/// enough that a build's serial and parallel phases show up as separate intervals.
pub(super) const SAMPLE: Duration = Duration::from_secs(5);

/// The longest interval credited to a shape. A longer one means the sampler was not running (the process was
/// stopped or the machine was held), and averaging it into one shape would misdescribe it: three sample periods
/// tolerates a late wake-up but not a missed stretch.
const MAX_GAP: Duration = Duration::from_secs(15);

/// Per thousand of an interval a core must run to count as busy: far above what desktop wake-ups and background
/// services use, and well below the near-total share of a core under sustained load, so a core that is working
/// counts even when scheduling noise takes a third of its interval.
const BUSY_PERMILLE: u64 = 600;

/// Per thousand of an interval below which a core counts as idle: timer ticks and housekeeping keep an idle core
/// above zero, and 5% is above them while still below any work a person or a service would call light.
const LIGHT_PERMILLE: u64 = 50;

/// Per thousand of the cores that may be busy for the interval to count as single-thread boost: a quarter, because
/// boost clocks hold near their limit only while a few cores are active.
const SINGLE_THREAD_PERMILLE: usize = 250;

/// Per thousand of the cores that must be busy for light work beside them to count as the R8 shape: half, the
/// least that makes the neighbours' heat and power draw the main condition of the lightly loaded core.
const NEIGHBOURS_PERMILLE: usize = 500;

/// The state of one core over one interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoreState {
    Idle,
    Light,
    Busy,
}

impl CoreState {
    fn of(permille: u64) -> Self {
        if permille >= BUSY_PERMILLE {
            Self::Busy
        } else if permille >= LIGHT_PERMILLE {
            Self::Light
        } else {
            Self::Idle
        }
    }
}

/// How many cores were in each state over one interval.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Census {
    pub idle: usize,
    pub light: usize,
    pub busy: usize,
}

impl Census {
    fn total(self) -> usize {
        self.idle + self.light + self.busy
    }

    fn add(&mut self, state: CoreState) {
        match state {
            CoreState::Idle => self.idle += 1,
            CoreState::Light => self.light += 1,
            CoreState::Busy => self.busy += 1,
        }
    }

    /// Whether at least `permille` per thousand of the cores are busy.
    fn busy_share_reaches(self, permille: usize) -> bool {
        self.busy * 1000 >= permille * self.total()
    }
}

/// The load shape of one interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Shape {
    /// A few cores busy, the rest idle or light.
    SingleThread,
    /// At least half the cores busy and at least one lightly loaded: the R8 shape.
    LightBesideBusy,
    /// Every core busy: the R9 shape.
    AllThreads,
    /// The machine went from quiet to active or back.
    IdleTransition,
    /// No core busy, after an interval with none.
    Quiet,
    /// Active, but none of the shapes above.
    Other,
}

/// The shape of an interval with this `census`. `was_quiet` is whether the interval before had no busy core, when
/// that is known: a change either way is an idle transition whatever else the census shows.
pub(super) fn classify(census: Census, was_quiet: Option<bool>) -> Shape {
    let quiet = census.busy == 0;
    if was_quiet.is_some_and(|before| before != quiet) {
        return Shape::IdleTransition;
    }
    if quiet {
        Shape::Quiet
    } else if census.busy == census.total() {
        Shape::AllThreads
    } else if census.light > 0 && census.busy_share_reaches(NEIGHBOURS_PERMILLE) {
        Shape::LightBesideBusy
    } else if census.busy * 1000 <= SINGLE_THREAD_PERMILLE * census.total() {
        Shape::SingleThread
    } else {
        Shape::Other
    }
}

/// Counts the cores' states from two readings of the cumulative times. A core takes its busiest thread's share; a
/// core none of whose threads advanced, or that is not in the readings, is left out.
fn census(cores: &[Vec<CpuId>], before: &[CpuTime], now: &[CpuTime]) -> Census {
    let before: HashMap<CpuId, &CpuTime> = before.iter().map(|time| (time.cpu, time)).collect();
    let mut census = Census::default();
    for threads in cores {
        let busiest = threads
            .iter()
            .filter_map(|cpu| {
                let then = before.get(cpu)?;
                let at = now.iter().find(|time| time.cpu == *cpu)?;
                let total = at.total.saturating_sub(then.total);
                let busy = at.busy.saturating_sub(then.busy).min(total);
                (total > 0).then(|| busy.saturating_mul(1000) / total)
            })
            .max();
        if let Some(permille) = busiest {
            census.add(CoreState::of(permille));
        }
    }
    census
}

/// Turns successive readings of the cumulative per-CPU times into a [`LoadMix`].
pub(super) struct Sampler {
    cores: Vec<Vec<CpuId>>,
    last: Option<(Duration, Vec<CpuTime>)>,
    was_quiet: Option<bool>,
    mix: LoadMix,
}

impl Sampler {
    /// Starts from the mix already recorded for the boot, so a restart of the service continues it.
    pub(super) fn new(cores: &[CoreInfo], mix: LoadMix) -> Self {
        Self {
            cores: cores.iter().map(|core| core.cpus.clone()).collect(),
            last: None,
            was_quiet: None,
            mix,
        }
    }

    pub(super) fn mix(&self) -> LoadMix {
        self.mix
    }

    /// Credits the time since the previous reading to the shape the readings show. `at` is the active monotonic
    /// time of this reading, which stands still while the machine is suspended.
    pub(super) fn record(&mut self, at: Duration, times: Vec<CpuTime>) -> Result<(), Error> {
        let Some((then, before)) = self.last.replace((at, times)) else {
            return Ok(());
        };
        let gap = at.checked_sub(then).ok_or(Error::ClockReversed)?;
        if gap > MAX_GAP {
            self.was_quiet = None;
            return Ok(());
        }
        let Some((_, now)) = &self.last else {
            return Ok(());
        };
        let census = census(&self.cores, &before, now);
        if census.total() == 0 {
            self.was_quiet = None;
            return Ok(());
        }
        let shape = classify(census, self.was_quiet);
        self.was_quiet = Some(census.busy == 0);
        let millis = u64::try_from(gap.as_millis()).map_err(|_| Error::DurationOverflow)?;
        let field = match shape {
            Shape::SingleThread => &mut self.mix.single_thread_ms,
            Shape::LightBesideBusy => &mut self.mix.light_beside_busy_ms,
            Shape::AllThreads => &mut self.mix.all_threads_ms,
            Shape::IdleTransition => &mut self.mix.idle_transition_ms,
            Shape::Quiet => &mut self.mix.quiet_ms,
            Shape::Other => &mut self.mix.other_ms,
        };
        *field = field.saturating_add(millis);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;
    use togi_machine::{CcdId, CoreId};

    use super::*;

    const fn census_of(idle: usize, light: usize, busy: usize) -> Census {
        Census { idle, light, busy }
    }

    #[test_case(census_of(16, 0, 0), None, Shape::Quiet; "all idle")]
    #[test_case(census_of(10, 6, 0), None, Shape::Quiet; "light work only is quiet")]
    #[test_case(census_of(15, 0, 1), None, Shape::SingleThread; "one busy core")]
    #[test_case(census_of(12, 0, 4), None, Shape::SingleThread; "a quarter busy")]
    #[test_case(census_of(11, 0, 5), None, Shape::Other; "just over a quarter busy, none light")]
    #[test_case(census_of(0, 1, 15), None, Shape::LightBesideBusy; "one light core beside fifteen busy")]
    #[test_case(census_of(7, 1, 8), None, Shape::LightBesideBusy; "half busy and one light")]
    #[test_case(census_of(8, 0, 8), None, Shape::Other; "half busy, the others idle")]
    #[test_case(census_of(0, 0, 16), None, Shape::AllThreads; "every core busy")]
    #[test_case(census_of(16, 0, 0), Some(false), Shape::IdleTransition; "going quiet")]
    #[test_case(census_of(15, 0, 1), Some(true), Shape::IdleTransition; "waking up")]
    #[test_case(census_of(0, 0, 16), Some(true), Shape::IdleTransition; "waking into all threads")]
    #[test_case(census_of(15, 0, 1), Some(false), Shape::SingleThread; "staying active")]
    #[test_case(census_of(16, 0, 0), Some(true), Shape::Quiet; "staying quiet")]
    fn an_interval_takes_one_shape(census: Census, was_quiet: Option<bool>, shape: Shape) {
        assert_eq!(classify(census, was_quiet), shape);
    }

    #[test_case(0, CoreState::Idle; "none")]
    #[test_case(49, CoreState::Idle; "just below light")]
    #[test_case(50, CoreState::Light; "light")]
    #[test_case(599, CoreState::Light; "just below busy")]
    #[test_case(600, CoreState::Busy; "busy")]
    #[test_case(1000, CoreState::Busy; "saturated")]
    fn a_core_state_follows_its_share_of_the_interval(permille: u64, state: CoreState) {
        assert_eq!(CoreState::of(permille), state);
    }

    fn core(id: u16, cpus: &[u16]) -> CoreInfo {
        CoreInfo {
            core: CoreId::new(i64::from(id)).unwrap(),
            ccd: CcdId::new(0).unwrap(),
            cpus: cpus.iter().copied().map(CpuId::from).collect(),
        }
    }

    /// Cumulative times that advance each CPU by `ticks` per reading, `busy[i]` of them busy.
    fn reading(step: u64, ticks: u64, busy: &[u64]) -> Vec<CpuTime> {
        busy.iter()
            .enumerate()
            .map(|(cpu, busy)| CpuTime {
                cpu: CpuId::from(u16::try_from(cpu).unwrap()),
                busy: busy * step,
                total: ticks * step,
            })
            .collect()
    }

    /// Four single-thread cores, then four SMT cores, sampled every `SAMPLE`.
    fn sampler() -> Sampler {
        Sampler::new(
            &[
                core(0, &[0, 1]),
                core(1, &[2, 3]),
                core(2, &[4]),
                core(3, &[5]),
            ],
            LoadMix::default(),
        )
    }

    fn at(step: u32) -> Duration {
        SAMPLE * step
    }

    #[test]
    fn a_core_takes_the_share_of_its_busiest_thread() {
        let mut sampler = sampler();
        // Core 0 runs one thread flat out and idles the other; the others idle.
        sampler.record(at(0), reading(0, 100, &[0; 6])).unwrap();
        sampler
            .record(at(1), reading(1, 100, &[100, 0, 0, 0, 0, 0]))
            .unwrap();
        assert_eq!(
            sampler.mix(),
            LoadMix {
                single_thread_ms: 5000,
                ..LoadMix::default()
            }
        );
    }

    #[test]
    fn intervals_accumulate_by_shape_and_the_first_reading_credits_nothing() {
        let mut sampler = sampler();
        let all_busy = [100; 6];
        let quiet = [0; 6];
        let one_busy = [100, 0, 0, 0, 0, 0];
        let one_light_beside_busy = [100, 100, 100, 100, 100, 30];
        // Cumulative counters: each reading adds one 100-tick step with the busy ticks below.
        let steps = [
            quiet,
            quiet,
            one_busy,
            one_busy,
            all_busy,
            one_light_beside_busy,
            quiet,
        ];
        let mut busy = [0_u64; 6];
        for (index, step) in steps.iter().enumerate() {
            for (total, add) in busy.iter_mut().zip(step) {
                *total += add;
            }
            let times = busy
                .iter()
                .enumerate()
                .map(|(cpu, busy)| CpuTime {
                    cpu: CpuId::from(u16::try_from(cpu).unwrap()),
                    busy: *busy,
                    total: 100 * u64::try_from(index + 1).unwrap(),
                })
                .collect();
            sampler
                .record(at(u32::try_from(index).unwrap()), times)
                .unwrap();
        }
        // Intervals 1..=6: quiet (first interval, no earlier state), transition (waking), single thread,
        // all threads, light beside busy, transition (going quiet).
        assert_eq!(
            sampler.mix(),
            LoadMix {
                single_thread_ms: 5000,
                light_beside_busy_ms: 5000,
                all_threads_ms: 5000,
                idle_transition_ms: 10_000,
                quiet_ms: 5000,
                other_ms: 0,
            }
        );
    }

    #[test]
    fn a_gap_longer_than_three_sample_periods_credits_nothing_and_forgets_the_idle_state() {
        let mut sampler = sampler();
        sampler.record(at(0), reading(0, 100, &[0; 6])).unwrap();
        sampler.record(at(1), reading(1, 100, &[0; 6])).unwrap();
        sampler.record(at(10), reading(10, 100, &[100; 6])).unwrap();
        assert_eq!(sampler.mix().total_ms(), 5000);
        // The interval after the gap is not an idle transition: the state before it is unknown.
        sampler.record(at(11), reading(11, 100, &[100; 6])).unwrap();
        assert_eq!(sampler.mix().all_threads_ms, 5000);
    }

    #[test]
    fn a_restart_continues_the_recorded_mix() {
        let before = LoadMix {
            all_threads_ms: 7000,
            ..LoadMix::default()
        };
        let mut sampler = Sampler::new(&[core(0, &[0])], before);
        sampler.record(at(0), reading(0, 100, &[0])).unwrap();
        sampler.record(at(1), reading(1, 100, &[100])).unwrap();
        assert_eq!(sampler.mix().all_threads_ms, 12_000);
    }

    #[test]
    fn a_clock_that_moves_backwards_is_refused() {
        let mut sampler = sampler();
        sampler.record(at(2), reading(2, 100, &[0; 6])).unwrap();
        assert!(matches!(
            sampler.record(at(1), reading(1, 100, &[0; 6])),
            Err(Error::ClockReversed)
        ));
    }

    #[test]
    fn a_core_whose_counters_did_not_advance_is_left_out() {
        let times = |ticks: u64| {
            vec![
                CpuTime {
                    cpu: CpuId::from(0_u16),
                    busy: ticks,
                    total: ticks,
                },
                CpuTime {
                    cpu: CpuId::from(1_u16),
                    busy: 0,
                    total: 0,
                },
            ]
        };
        let cores = vec![vec![CpuId::from(0_u16)], vec![CpuId::from(1_u16)]];
        assert_eq!(census(&cores, &times(0), &times(100)), census_of(0, 0, 1));
    }

    #[test]
    fn an_interval_in_which_no_counter_advanced_credits_nothing_and_forgets_the_last_shape() {
        let mut sampler = sampler();
        sampler.record(at(0), reading(0, 100, &[0; 6])).unwrap();
        sampler.record(at(1), reading(1, 100, &[0; 6])).unwrap();
        let credited = sampler.mix();
        assert_eq!(credited.quiet_ms, 5000);
        // The counters stand still for an interval, so it says nothing about the load.
        sampler.record(at(2), reading(1, 100, &[0; 6])).unwrap();
        assert_eq!(sampler.mix(), credited);
        // The next interval is busy; with the quiet one forgotten it is no idle transition.
        sampler.record(at(3), reading(2, 100, &[50; 6])).unwrap();
        assert_eq!(
            sampler.mix(),
            LoadMix {
                all_threads_ms: 5000,
                ..credited
            }
        );
    }
}
