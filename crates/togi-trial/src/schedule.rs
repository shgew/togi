use std::iter::Peekable;
use std::time::Duration;

use togi_machine::LoadSchedule;

use crate::plan::Load;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Stop,
    Cont,
}

/// One scheduled signal, `at` measured from the start of timing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Step {
    pub(crate) at: Duration,
    pub(crate) instance: usize,
    pub(crate) action: Action,
}

pub(crate) const BURST_LENGTH: Duration = Duration::from_millis(100);
pub(crate) const BURST_PERIOD: Duration = Duration::from_secs(2);

/// The signals the instances' load shapes send strictly before `duration`, lazily and in time
/// order. `loads` is indexed like the trial plan's instances, and every instance follows its own
/// shape independently of the others; steps due at the same time come out in instance order, and
/// all bursts share one rotation by their positions.
pub(crate) fn timeline(loads: &[Load], duration: Duration) -> Box<dyn Iterator<Item = Step>> {
    let mut streams: Vec<Box<dyn Iterator<Item = Step>>> = Vec::new();
    let mut bursting: Vec<(usize, usize)> = Vec::new();
    for (instance, load) in loads.iter().enumerate() {
        match *load {
            Load::Continuous => {}
            Load::Steps(schedule) => streams.push(steps(instance, schedule, duration)),
            Load::Burst { position } => bursting.push((position, instance)),
            Load::Phases { phase } => streams.push(phases(instance, phase, duration)),
        }
    }
    if !bursting.is_empty() {
        bursting.sort_unstable();
        let order = bursting.into_iter().map(|(_, instance)| instance).collect();
        streams.push(bursts(order, duration));
    }
    match streams.len() {
        0 => Box::new(std::iter::empty()),
        1 => streams.swap_remove(0),
        _ => Box::new(Merged {
            streams: streams.into_iter().map(Iterator::peekable).collect(),
        }),
    }
}

/// Time-ordered merge of independent step streams; the earlier stream wins a tie.
struct Merged {
    streams: Vec<Peekable<Box<dyn Iterator<Item = Step>>>>,
}

impl Iterator for Merged {
    type Item = Step;

    fn next(&mut self) -> Option<Step> {
        let mut earliest: Option<(usize, Duration)> = None;
        for (index, stream) in self.streams.iter_mut().enumerate() {
            if let Some(step) = stream.peek()
                && earliest.is_none_or(|(_, at)| step.at < at)
            {
                earliest = Some((index, step.at));
            }
        }
        self.streams[earliest?.0].next()
    }
}

fn steps(
    instance: usize,
    schedule: LoadSchedule,
    duration: Duration,
) -> Box<dyn Iterator<Item = Step>> {
    let mut elapsed = Duration::ZERO;
    Box::new(
        schedule
            .periods()
            .enumerate()
            .map(move |(index, period)| {
                elapsed += period;
                Step {
                    at: elapsed,
                    instance,
                    action: if index % 2 == 0 {
                        Action::Stop
                    } else {
                        Action::Cont
                    },
                }
            })
            .take_while(move |step| step.at < duration),
    )
}

/// Stops the instance after `phase`, continues it after two, and so on.
fn phases(instance: usize, phase: Duration, duration: Duration) -> Box<dyn Iterator<Item = Step>> {
    if phase.is_zero() {
        return Box::new(std::iter::empty());
    }
    Box::new(
        (1_u32..)
            .map_while(move |n| phase.checked_mul(n).map(|at| (n, at)))
            .take_while(move |(_, at)| *at < duration)
            .map(move |(n, at)| Step {
                at,
                instance,
                action: if n % 2 == 1 {
                    Action::Stop
                } else {
                    Action::Cont
                },
            }),
    )
}

fn bursts(order: Vec<usize>, duration: Duration) -> Box<dyn Iterator<Item = Step>> {
    if order.is_empty() {
        return Box::new(std::iter::empty());
    }
    let half = duration / 2;
    Box::new(
        (0_u32..)
            .map(move |burst| (burst, half + BURST_PERIOD * burst))
            .take_while(move |(_, at)| *at < duration)
            .flat_map(move |(burst, at)| {
                let instance = order[burst as usize % order.len()];
                let cont = Step {
                    at,
                    instance,
                    action: Action::Cont,
                };
                let stop = Step {
                    at: at + BURST_LENGTH,
                    instance,
                    action: Action::Stop,
                };
                [Some(cont), (stop.at < duration).then_some(stop)]
                    .into_iter()
                    .flatten()
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use togi_machine::LoadSchedule;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn continuous_load_sends_nothing() {
        assert_eq!(timeline(&[Load::Continuous], ms(10_000)).count(), 0);
    }

    #[test]
    fn duty_schedule_alternates_stop_and_cont_before_the_end() {
        let load = Load::Steps(LoadSchedule::Duty {
            pct: togi_machine::DutyPct::new(50).unwrap(),
        });
        let steps: Vec<_> = timeline(&[load], ms(260)).collect();
        let times: Vec<_> = steps.iter().map(|s| s.at).collect();
        assert_eq!(times, [ms(50), ms(100), ms(150), ms(200), ms(250)]);
        let actions: Vec<_> = steps.iter().map(|s| s.action).collect();
        assert_eq!(
            actions,
            [
                Action::Stop,
                Action::Cont,
                Action::Stop,
                Action::Cont,
                Action::Stop
            ]
        );
    }

    #[test]
    fn a_step_at_the_exact_end_is_not_sent() {
        let load = Load::Steps(LoadSchedule::Duty {
            pct: togi_machine::DutyPct::new(50).unwrap(),
        });
        assert_eq!(timeline(&[load], ms(100)).count(), 1);
    }

    #[test]
    fn random_schedule_is_reproducible_and_prefix_stable() {
        let load = [Load::Steps(LoadSchedule::Random { seed: 42 })];
        let long: Vec<_> = timeline(&load, Duration::from_secs(120)).collect();
        let short: Vec<_> = timeline(&load, Duration::from_secs(30)).collect();
        assert!(short.len() < long.len());
        assert_eq!(short[..], long[..short.len()]);
        assert_eq!(
            long,
            timeline(&load, Duration::from_secs(120)).collect::<Vec<_>>()
        );
        assert!(long.windows(2).all(|w| w[0].at < w[1].at));
    }

    #[test]
    fn bursts_start_after_half_the_trial_one_core_at_a_time() {
        // The rotation visits instance 2, then 0, then 1.
        let loads = [
            Load::Burst { position: 1 },
            Load::Burst { position: 2 },
            Load::Burst { position: 0 },
        ];
        let steps: Vec<_> = timeline(&loads, Duration::from_secs(10)).collect();
        let shape: Vec<_> = steps.iter().map(|s| (s.at, s.instance, s.action)).collect();
        assert_eq!(
            shape,
            [
                (Duration::from_secs(5), 2, Action::Cont),
                (Duration::from_millis(5100), 2, Action::Stop),
                (Duration::from_secs(7), 0, Action::Cont),
                (Duration::from_millis(7100), 0, Action::Stop),
                (Duration::from_secs(9), 1, Action::Cont),
                (Duration::from_millis(9100), 1, Action::Stop),
            ]
        );
    }

    #[test]
    fn a_burst_stop_reaching_the_end_is_omitted_and_cores_cycle() {
        let loads = [Load::Continuous, Load::Burst { position: 0 }];
        let steps: Vec<_> = timeline(&loads, ms(4050)).collect();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[2].action, Action::Cont);
        assert_eq!(steps[2].at, ms(4025));
    }

    #[test]
    fn consumers_may_stop_early() {
        let mut steps = timeline(&[Load::Burst { position: 0 }], Duration::from_secs(10));
        assert!(steps.next().is_some());
        assert!(steps.next().is_some());
        drop(steps);
    }

    #[test]
    fn phases_stop_after_one_phase_and_continue_after_two() {
        let load = Load::Phases {
            phase: Duration::from_secs(10),
        };
        let shape: Vec<_> = timeline(&[load], Duration::from_secs(40))
            .map(|s| (s.at, s.action))
            .collect();
        assert_eq!(
            shape,
            [
                (Duration::from_secs(10), Action::Stop),
                (Duration::from_secs(20), Action::Cont),
                (Duration::from_secs(30), Action::Stop),
            ]
        );
    }

    #[test]
    fn a_phase_ending_at_the_deadline_is_not_sent() {
        let load = Load::Phases {
            phase: Duration::from_secs(10),
        };
        assert_eq!(timeline(&[load], Duration::from_secs(10)).count(), 0);
    }

    #[test]
    fn instances_follow_their_own_shapes_merged_in_time_order() {
        let loads = [
            Load::Continuous,
            Load::Steps(LoadSchedule::Duty {
                pct: togi_machine::DutyPct::new(50).unwrap(),
            }),
            Load::Phases {
                phase: Duration::from_millis(100),
            },
        ];
        let steps: Vec<_> = timeline(&loads, ms(260)).collect();
        assert!(steps.windows(2).all(|w| w[0].at <= w[1].at));
        assert!(steps.iter().all(|s| s.instance != 0));
        let of = |instance| -> Vec<_> {
            steps
                .iter()
                .filter(|s| s.instance == instance)
                .map(|s| s.at)
                .collect()
        };
        assert_eq!(of(1), [ms(50), ms(100), ms(150), ms(200), ms(250)]);
        assert_eq!(of(2), [ms(100), ms(200)]);
        let tied: Vec<_> = steps.iter().filter(|s| s.at == ms(100)).collect();
        assert_eq!(
            tied.iter().map(|s| s.instance).collect::<Vec<_>>(),
            [1, 2],
            "a tie goes to the earlier instance"
        );
    }
}
