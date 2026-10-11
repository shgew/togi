//! Checking cycles: the schedule a cycle captures, its R7 chains, the passes each requirement
//! has earned, and the ordinary attribution, backoff and reruns that go with them. Every count
//! and every chain is maintained when the event that changes it folds; the decision functions
//! only read.

mod chains;
mod cycle;
mod ordinary;
mod reruns;
mod schedule;

use std::collections::BTreeMap;
use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{CheckingChain, CheckingCycle, CheckingStep, CycleEvent, Event, Seq};
use togi_machine::Regime;

use crate::state::Run;
use crate::{ClassKey, FoldError, Profile, Topology, Window};

pub(crate) use chains::{Node, R7Step};
pub(crate) use reruns::Reruns;
pub(crate) use schedule::{Quota, StepPlan};

/// A cycle that is open: the schedule its start captured, the R7 occurrences that began, and for
/// each class of the schedule the live passes since the start that cover the checking profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OpenCycle {
    pub(crate) number: NonZeroU32,
    pub(crate) start: Seq,
    steps: Box<[Regime]>,
    r7: BTreeMap<usize, R7Step>,
    plan: Vec<StepPlan>,
    valid: BTreeMap<ClassKey, u32>,
    done: usize,
}

impl OpenCycle {
    fn have(&self, class: ClassKey) -> u32 {
        self.valid.get(&class).copied().unwrap_or(0)
    }

    fn met(&self, quota: &Quota) -> bool {
        self.have(quota.class) >= quota.count
    }

    fn first_incomplete(&self, topo: &Topology) -> usize {
        self.plan
            .iter()
            .enumerate()
            .position(|(index, step)| {
                let chains = step.regime != Regime::R7
                    || self.r7.get(&index).is_some_and(|started| {
                        started.complete(topo.ccds().iter().map(|&(ccd, _)| ccd))
                    });
                !chains || !step.quotas().all(|quota| self.met(quota))
            })
            .unwrap_or(self.plan.len())
    }

    /// The steps from the first that is not finished on.
    pub(crate) fn steps_done(&self) -> usize {
        self.done
    }
}

/// The cycle that ended last: its steps and how many of them were finished.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Closed {
    pub(crate) steps: Box<[Regime]>,
    pub(crate) steps_done: usize,
}

/// The hottest passing together trial since the checking profile changed, the earliest at a tie.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Peak {
    pub(crate) celsius: i32,
    pub(crate) seq: Seq,
}

/// The checking profile the last `profile.change` set, and the cycle checking is in.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Checking {
    profile: Option<Profile>,
    seq: Option<Seq>,
    last_cycle: Option<NonZeroU32>,
    open: Option<OpenCycle>,
    closed: Option<Closed>,
    last_seq: Option<Seq>,
    peak: Option<Peak>,
}

impl Checking {
    pub(crate) fn profile(&self) -> Option<Profile> {
        self.profile
    }

    pub(crate) fn seq(&self) -> Option<Seq> {
        self.seq
    }

    pub(crate) fn open(&self) -> Option<&OpenCycle> {
        self.open.as_ref()
    }

    pub(crate) fn closed(&self) -> Option<&Closed> {
        self.closed.as_ref()
    }

    pub(crate) fn last_cycle(&self) -> Option<NonZeroU32> {
        self.last_cycle
    }

    pub(crate) fn last_seq(&self) -> Option<Seq> {
        self.last_seq
    }

    pub(crate) fn temperature_peak(&self) -> Option<Peak> {
        self.peak
    }

    pub(crate) fn note_temperature(&mut self, celsius: i32, seq: Seq) {
        if self.peak.is_none_or(|peak| celsius > peak.celsius) {
            self.peak = Some(Peak { celsius, seq });
        }
    }

    /// Where the open cycle holds `class`: its step and part indexes, an R7 step only once it
    /// began. With `step`, only that step is asked.
    #[cfg(test)]
    pub(crate) fn locate(&self, class: ClassKey, step: Option<usize>) -> Option<(usize, usize)> {
        let open = self.open.as_ref()?;
        open.plan
            .iter()
            .enumerate()
            .filter(|&(index, plan)| {
                step.is_none_or(|only| only == index)
                    && (plan.regime != Regime::R7 || open.r7.contains_key(&index))
            })
            .find_map(|(index, plan)| {
                plan.parts
                    .iter()
                    .position(|part| {
                        part.quotas
                            .iter()
                            .any(|quota| quota.class == class && quota.count > 0)
                    })
                    .map(|part| (index, part))
            })
    }

    #[cfg(test)]
    pub(crate) fn counts(&self) -> Option<&BTreeMap<ClassKey, u32>> {
        self.open.as_ref().map(|open| &open.valid)
    }
}

fn step_index(step: NonZeroU32) -> usize {
    usize::try_from(step.get() - 1).unwrap_or(usize::MAX)
}

impl Run {
    fn cycle_count(&self, open: &OpenCycle, class: ClassKey) -> u32 {
        let Some(profile) = self.checking.profile else {
            return 0;
        };
        self.evidence
            .count(
                class,
                &profile,
                Window::Live {
                    since: Some(open.start),
                },
                NonZeroU16::MIN,
            )
            .map_or(0, |count| count.valid)
    }

    /// Recomputes the open cycle's schedule and every count from its steps, chains, durations and
    /// the evidence; asked when one of those changes.
    pub(crate) fn rebuild_cycle(&mut self) {
        let Some(mut open) = self.checking.open.take() else {
            return;
        };
        open.plan = schedule::build(&self.config, &self.topo, &open.steps, &open.r7);
        open.valid = open
            .plan
            .iter()
            .flat_map(StepPlan::quotas)
            .map(|quota| (quota.class, self.cycle_count(&open, quota.class)))
            .collect();
        open.done = open.first_incomplete(&self.topo);
        self.checking.open = Some(open);
    }

    fn refresh_cycle(&mut self, touched: impl Fn(ClassKey) -> bool) {
        let Some(mut open) = self.checking.open.take() else {
            return;
        };
        let classes: Vec<ClassKey> = open
            .valid
            .keys()
            .copied()
            .filter(|&class| touched(class))
            .collect();
        for class in classes {
            let count = self.cycle_count(&open, class);
            open.valid.insert(class, count);
        }
        open.done = open.first_incomplete(&self.topo);
        self.checking.open = Some(open);
    }

    /// A trial of `class` ended with evidence, or a carried failure of it came in.
    pub(crate) fn cycle_evidence_changed(&mut self, class: ClassKey) {
        self.refresh_cycle(|tracked| tracked == class);
    }

    /// An idle failure invalidates every all-core R6 class.
    pub(crate) fn cycle_idle_failed(&mut self) {
        let all = self.topo.all();
        self.refresh_cycle(|tracked| tracked.regime() == Regime::R6 && tracked.loaded() == all);
    }

    pub(crate) fn cycle_evidence_reset(&mut self) {
        self.refresh_cycle(|_| true);
    }

    pub(crate) fn checking_cycle(
        &mut self,
        event: &Event,
        cycle: &CheckingCycle,
    ) -> Result<(), FoldError> {
        match cycle.event {
            CycleEvent::Start => {
                self.phases.cycle_start(event.seq);
                self.later_cycle_start(cycle.cycle);
                self.checking.open = Some(OpenCycle {
                    number: cycle.cycle,
                    start: event.seq,
                    steps: cycle.steps.as_slice().into(),
                    r7: BTreeMap::new(),
                    plan: Vec::new(),
                    valid: BTreeMap::new(),
                    done: 0,
                });
                self.checking.last_cycle = Some(cycle.cycle);
                self.checking.last_seq = Some(event.seq);
                self.rebuild_cycle();
            }
            CycleEvent::End => {
                if cycle.passed && cycle.full {
                    let searching = self.any_searching();
                    self.hunt_passed_full_cycle(event.seq);
                    self.phases.passed_full_cycle(
                        event.seq,
                        cycle.cycle,
                        searching,
                        self.checking.profile,
                    )?;
                    self.later_full_cycle(event.seq, cycle.cycle);
                }
                self.checking.closed = self.checking.open.take().map(|open| Closed {
                    steps_done: if cycle.passed {
                        open.steps.len()
                    } else {
                        open.done
                    },
                    steps: open.steps,
                });
                self.checking.last_seq = Some(event.seq);
            }
        }
        self.reruns.consume_citations();
        self.revalidate_retry();
        Ok(())
    }

    pub(crate) fn checking_step(
        &mut self,
        event: &Event,
        step: &CheckingStep,
    ) -> Result<(), FoldError> {
        Profile::new(&self.topo, step.profile)?;
        let index = step_index(step.step);
        let Some(open) =
            self.checking.open.as_mut().filter(|open| {
                open.number == step.cycle && open.steps.get(index) == Some(&Regime::R7)
            })
        else {
            return Ok(());
        };
        open.r7.insert(index, R7Step::new(event.seq));
        self.checking.last_seq = Some(event.seq);
        self.rebuild_cycle();
        self.revalidate_retry();
        Ok(())
    }

    pub(crate) fn checking_chain(
        &mut self,
        event: &Event,
        chain: &CheckingChain,
    ) -> Result<(), FoldError> {
        let profile = Profile::new(&self.topo, chain.profile)?;
        self.require_cores(chain.cores)?;
        if !self.topo.ccds().iter().any(|&(ccd, _)| ccd == chain.ccd) {
            return Err(FoldError::UnknownCcd(chain.ccd));
        }
        let index = step_index(chain.step);
        let current = self.checking.profile;
        let Some(open) = self
            .checking
            .open
            .as_mut()
            .filter(|open| open.number == chain.cycle)
        else {
            return Ok(());
        };
        let Some(started) = open.r7.get_mut(&index) else {
            return Ok(());
        };
        if open
            .plan
            .get(index)
            .is_none_or(|plan| plan.workload != chain.workload)
        {
            return Ok(());
        }
        started.record(
            chain.ccd,
            Node {
                part: chain.part,
                cores: chain.cores,
                profile,
                seq: event.seq,
            },
            current.as_ref(),
        );
        self.checking.last_seq = Some(event.seq);
        self.rebuild_cycle();
        self.revalidate_retry();
        Ok(())
    }

    /// The checking profile changed: derived chains stand only where they were derived.
    pub(crate) fn checking_profile(&mut self, event: &Event, profile: Profile) {
        self.checking.profile = Some(profile);
        self.checking.seq = Some(event.seq);
        self.checking.last_seq = Some(event.seq);
        self.checking.peak = None;
        if let Some(open) = self.checking.open.as_mut() {
            for started in open.r7.values_mut() {
                started.retarget(&profile);
            }
        }
        self.rebuild_cycle();
    }
}
