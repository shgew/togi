use std::time::Duration;

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

/// The signals a load shape sends strictly before `duration`, lazily and in time order.
pub(crate) fn timeline(load: &Load, duration: Duration) -> Box<dyn Iterator<Item = Step>> {
    match load {
        Load::Continuous => Box::new(std::iter::empty()),
        Load::Steps(schedule) => {
            let mut elapsed = Duration::ZERO;
            Box::new(
                schedule
                    .periods()
                    .enumerate()
                    .map(move |(index, period)| {
                        elapsed += period;
                        Step {
                            at: elapsed,
                            instance: 0,
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
        Load::Bursts { order } => bursts(order.clone(), duration),
    }
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
        assert_eq!(timeline(&Load::Continuous, ms(10_000)).count(), 0);
    }

    #[test]
    fn duty_schedule_alternates_stop_and_cont_before_the_end() {
        let load = Load::Steps(LoadSchedule::Duty { pct: 50 });
        let steps: Vec<_> = timeline(&load, ms(260)).collect();
        let times: Vec<_> = steps.iter().map(|s| s.at).collect();
        assert_eq!(times, [ms(50), ms(100), ms(150), ms(200), ms(250)]);
        let actions: Vec<_> = steps.iter().map(|s| s.action).collect();
        assert_eq!(
            actions,
            [Action::Stop, Action::Cont, Action::Stop, Action::Cont, Action::Stop]
        );
    }

    #[test]
    fn a_step_at_the_exact_end_is_not_sent() {
        let load = Load::Steps(LoadSchedule::Duty { pct: 50 });
        assert_eq!(timeline(&load, ms(100)).count(), 1);
    }

    #[test]
    fn random_schedule_is_reproducible_and_prefix_stable() {
        let load = Load::Steps(LoadSchedule::Random { seed: 42 });
        let long: Vec<_> = timeline(&load, Duration::from_secs(120)).collect();
        let short: Vec<_> = timeline(&load, Duration::from_secs(30)).collect();
        assert!(short.len() < long.len());
        assert_eq!(short[..], long[..short.len()]);
        assert_eq!(long, timeline(&load, Duration::from_secs(120)).collect::<Vec<_>>());
        assert!(long.windows(2).all(|w| w[0].at < w[1].at));
    }

    #[test]
    fn bursts_start_after_half_the_trial_one_core_at_a_time() {
        let load = Load::Bursts {
            order: vec![2, 0, 1],
        };
        let steps: Vec<_> = timeline(&load, Duration::from_secs(10)).collect();
        let shape: Vec<_> = steps
            .iter()
            .map(|s| (s.at, s.instance, s.action))
            .collect();
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
        let load = Load::Bursts { order: vec![1] };
        let steps: Vec<_> = timeline(&load, ms(4050)).collect();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[2].action, Action::Cont);
        assert_eq!(steps[2].at, ms(4025));
        let none = Load::Bursts { order: vec![] };
        assert_eq!(timeline(&none, ms(4050)).count(), 0);
    }

    #[test]
    fn consumers_may_stop_early() {
        let load = Load::Bursts { order: vec![0] };
        let mut steps = timeline(&load, Duration::from_secs(10));
        assert!(steps.next().is_some());
        assert!(steps.next().is_some());
        drop(steps);
    }
}
