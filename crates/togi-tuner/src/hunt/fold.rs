use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{
    Activity, Combination, Event, HuntEnd, HuntGroup, HuntOutcome, HuntStage, HuntStart, Seq,
    TunerDecision,
};
use togi_machine::{CoreId, CoreSet, Regime, TargetOffset};

use super::plan::{GroupOutcome, Located as PlanLocated, Plan, Start, already_broken};
use super::state::{
    Active, Cut, Ended, GroupRecord, LoadedEnd, Located, LocatedEnd, Named, ParkedFailure,
    Recorded, Rejected, Repeated,
};
use crate::r7::Applied;
use crate::state::Run;
use crate::{ClassKey, CombinationId, FoldError, Profile, Ranking, Secs};

fn secs(value: u32) -> Result<Secs, FoldError> {
    NonZeroU32::new(value)
        .map(Secs::new)
        .ok_or(FoldError::ZeroDuration)
}

impl Run {
    pub(crate) fn hunt_needed(trials: u32) -> NonZeroU16 {
        u16::try_from(trials)
            .ok()
            .and_then(NonZeroU16::new)
            .unwrap_or(if trials == 0 {
                NonZeroU16::MIN
            } else {
                NonZeroU16::MAX
            })
    }

    fn duration_prior(
        &self,
        start: &HuntStart,
        class: ClassKey,
        failing: &Profile,
        needed: NonZeroU16,
        short: Secs,
    ) -> Vec<Seq> {
        if start.trial.is_none() || start.duration_s <= short.get() {
            return Vec::new();
        }
        let reset = self.hunts.reset;
        if self
            .evidence
            .failed_before(class.regime(), short, reset, start.failure)
        {
            return Vec::new();
        }
        let class = ClassKey::new(class.workload(), class.loaded(), short);
        let Ok(count) = self.evidence.count(
            class,
            failing,
            crate::Window::LiveOrCarried { since: reset },
            needed,
        ) else {
            return Vec::new();
        };
        let enough = count.witnesses.len() >= usize::from(needed.get())
            && count
                .witnesses
                .last()
                .is_some_and(|&seq| seq < start.failure);
        if enough { count.witnesses } else { Vec::new() }
    }

    fn repeated_core(
        &self,
        start: &HuntStart,
        class: ClassKey,
        failing: &Profile,
        recent: CoreSet,
    ) -> Option<Repeated> {
        if start.candidates.len() <= 2 || recent.len() != 1 {
            return None;
        }
        let core = recent.first()?;
        if !start.candidates.contains(core) {
            return None;
        }
        let parked = self.hunts.parked_failures.get(&class)?;
        let [newer, older] = parked.newest_two_before(start.failure)?;
        let adjacent = newer.core == core
            && older.core == core
            && older.offset.get() + 1 == newer.offset.get()
            && failing.at(core).get() == newer.offset.get() + 1;
        adjacent.then_some(Repeated {
            core,
            failures: [older.seq, newer.seq],
        })
    }

    pub(crate) fn hunt_start(&mut self, event: &Event, start: &HuntStart) -> Result<(), FoldError> {
        let workload = start.workload;
        if workload.regime() != start.regime {
            return Err(FoldError::WorkloadRegime {
                regime: start.regime,
                workload,
            });
        }
        self.require_cores(start.cores)?;
        self.require_cores(start.candidates)?;
        let failing = Profile::new(&self.topo, start.failing)?;
        let parked = Profile::new(&self.topo, start.parked)?;
        let loaded = if start.cores.is_empty() {
            self.topo.all()
        } else {
            start.cores
        };
        let duration = secs(start.duration_s)?;
        let short = secs(start.trial_s)?;
        let class = ClassKey::new(workload, loaded, duration);
        let needed = Self::hunt_needed(start.trials);
        let recent = self.constraints.recent();
        let ranking = Ranking::new(&self.topo, start.ranking.clone())?;
        let located = start.regime == Regime::R7
            && loaded.len() > 1
            && !start.candidates.iter().any(|core| loaded.contains(core));
        let prior = self.duration_prior(start, class, &failing, needed, short);
        let first_duration = if prior.is_empty() { short } else { duration };
        let repeated = self.repeated_core(
            start,
            ClassKey::new(workload, loaded, first_duration),
            &failing,
            recent,
        );
        let loaded_at_zero = loaded
            .iter()
            .all(|core| failing.at(core) == TargetOffset::ZERO);
        let plan = Plan::new(Start {
            candidates: start.candidates,
            loaded,
            recent,
            longer_failure: duration > short,
            duration_prior: !prior.is_empty(),
            located: located.then_some(PlanLocated { loaded_at_zero }),
            repeated_core: repeated.map(|repeated| repeated.core),
        })
        .map_err(|_| FoldError::NoCandidates(start.hunt))?;
        let active = Active {
            id: start.hunt,
            seq: event.seq,
            start: start.clone(),
            failing,
            parked,
            class,
            needed,
            short,
            recent,
            ranking,
            located,
            prior,
            repeated,
            plan,
            groups: Vec::new(),
            end: None,
            cut: Cut::default(),
            paired: self.later.release(&event.cause, loaded),
        };
        self.hunts.next_id = self.hunts.next_id.max(start.hunt.get());
        self.last_plan_seq = Some(event.seq);
        if located {
            self.hunts.located.insert(
                start.failure,
                Located {
                    hunt: start.hunt,
                    source: start.failure,
                    end: LocatedEnd::Open,
                },
            );
        } else {
            self.failures.dequeue_head(start.failure);
        }
        self.hunts.active = Some(Box::new(active));
        self.revalidate_retry();
        Ok(())
    }

    pub(crate) fn hunt_group(&mut self, event: &Event, group: &HuntGroup) -> Result<(), FoldError> {
        let Some(active) = self
            .hunts
            .active
            .as_ref()
            .filter(|active| active.id == group.hunt)
        else {
            return Ok(());
        };
        self.require_cores(group.cores)?;
        self.require_cores(group.set)?;
        let profile = Profile::new(&self.topo, group.profile)?;
        let class = active.class_at(secs(group.duration_s)?);
        let mut record = GroupRecord {
            seq: event.seq,
            payload: group.clone(),
            profile,
            class,
            inherited: event
                .cause
                .iter()
                .copied()
                .filter(|&seq| self.is_carried(seq))
                .collect(),
            own: None,
            closing: None,
        };
        if group.inferred.is_some() {
            record.own = Some(self.inferred_evidence(active, &record));
        }
        let replaces = active
            .latest()
            .is_some_and(|latest| latest.payload.group == group.group);
        let closing = (!replaces)
            .then(|| {
                active.groups.last().map(|latest| {
                    let mut answer = self.answer_of(active, latest);
                    answer.passes = self.passes_of(active, latest);
                    let advance = !latest.is_probe() && group.probe.is_none();
                    (answer, advance)
                })
            })
            .flatten();
        let Some(active) = self.hunts.active.as_mut() else {
            return Ok(());
        };
        if replaces {
            if let Some(slot) = active.groups.last_mut() {
                record.closing = slot.closing.take();
                *slot = record;
            }
        } else {
            if let Some((answer, advance)) = closing {
                if advance {
                    active
                        .plan
                        .advance(answer.outcome.unwrap_or(GroupOutcome::Skipped));
                }
                if let Some(latest) = active.groups.last_mut() {
                    latest.closing = Some(answer);
                }
            }
            if matches!(group.stage, HuntStage::Part | HuntStage::Complement)
                && group.probe.is_none()
            {
                let signature = (group.set, group.granularity, group.duration_s);
                if active.cut.signature != Some(signature) {
                    active.cut.count += 1;
                    active.cut.signature = Some(signature);
                }
            }
            active.groups.push(record);
        }
        self.revalidate_retry();
        Ok(())
    }

    pub(crate) fn hunt_end(&mut self, event: &Event, end: &HuntEnd) -> Result<(), FoldError> {
        self.require_cores(end.cores)?;
        let Some(active) = self
            .hunts
            .active
            .as_ref()
            .filter(|active| active.id == end.hunt)
        else {
            return Ok(());
        };
        let source = active.start.failure;
        let located = active.located;
        let direct_failure = (end.result == HuntOutcome::Direct)
            .then(|| event.cause.first().copied())
            .flatten();
        let loaded_end = (located && end.result == HuntOutcome::Loaded)
            .then(|| self.located_loaded(active, event.seq));
        if let Some(active) = self.hunts.active.as_mut() {
            active.end = Some(Ended {
                seq: event.seq,
                payload: end.clone(),
                direct_failure,
                combination: None,
            });
        }
        if located {
            self.hunt_located_end(source, event.seq, end.result, loaded_end);
        } else if end.result == HuntOutcome::Cancelled {
            self.failures.requeue(source);
            self.hunts.active = None;
        } else {
            self.failures.retire(source);
        }
        self.revalidate_retry();
        Ok(())
    }

    fn hunt_located_end(
        &mut self,
        source: Seq,
        seq: Seq,
        result: HuntOutcome,
        loaded: Option<LoadedEnd>,
    ) {
        match result {
            HuntOutcome::Cancelled => {
                self.hunts.located.remove(&source);
                self.hunts.active = None;
            }
            HuntOutcome::Loaded => {
                self.r7_located_loaded(source, seq);
                if let (Some(entry), Some(end)) = (self.hunts.located.get_mut(&source), loaded) {
                    entry.end = LocatedEnd::Loaded(end);
                }
                self.hunts.active = None;
            }
            HuntOutcome::Direct
            | HuntOutcome::Culprit
            | HuntOutcome::Combination
            | HuntOutcome::Fallback => {
                if let Some(entry) = self.hunts.located.get_mut(&source) {
                    entry.end = LocatedEnd::Consumed;
                }
                self.failures.retire(source);
                self.r7.open.remove(&source);
            }
        }
    }

    fn located_loaded(&self, active: &Active, seq: Seq) -> LoadedEnd {
        let all_zero = Self::loaded_all_zero(active);
        let rejected = self.rejecting_failure(active).map(|failure| Rejected {
            failure,
            named: if active.groups.len() > 1 && all_zero {
                None
            } else {
                self.loaded_core_named(active, failure)
            },
        });
        LoadedEnd {
            seq,
            all_zero,
            rejected,
        }
    }

    /// The loaded core the rejecting failure `failure` named, with the failed trial it reads.
    fn loaded_core_named(&self, active: &Active, failure: Seq) -> Option<Named> {
        let witness = self.failures.witnessed(failure)?;
        let core = witness
            .failure
            .core
            .filter(|&core| active.class.loaded().contains(core))?;
        let rejecting = self
            .hunts
            .rejecting
            .clone()
            .filter(|rejecting| rejecting.ledger == failure)?;
        Some(Named {
            core,
            failure: Box::new(rejecting.open),
        })
    }

    pub(crate) fn loaded_all_zero(active: &Active) -> bool {
        active
            .class
            .loaded()
            .iter()
            .all(|core| active.failing.at(core) == TargetOffset::ZERO)
    }

    pub(crate) fn rejecting_failure(&self, active: &Active) -> Option<Seq> {
        let latest = active.latest()?;
        if latest.payload.skipped || latest.payload.inferred.is_some() {
            return None;
        }
        self.answer_of(active, latest).failure
    }

    pub(crate) fn hunt_combination(&mut self, event: &Event, combination: &Combination) {
        self.hunts.next_combination = self
            .hunts
            .next_combination
            .max(combination.combination.get());
        let checking = self.checking.profile();
        let Some(active) = self.hunts.active.as_mut() else {
            return;
        };
        let Some(end) = active
            .end
            .as_mut()
            .filter(|_| active.id == combination.hunt)
        else {
            return;
        };
        let members = combination
            .members
            .iter()
            .map(|member| (member.core, member.offset))
            .collect();
        end.combination = Some(Recorded {
            seq: event.seq,
            id: CombinationId::new(combination.combination),
            members,
        });
        let broken = checking.is_some_and(|profile| {
            end.combination
                .as_ref()
                .is_some_and(|recorded| already_broken(&recorded.members, &profile))
        });
        if broken {
            self.hunt_close();
        }
    }

    pub(crate) fn hunt_decided(&mut self, event: &Event, decision: &TunerDecision) {
        let commits = decision.activity == Activity::Hunt
            && event
                .cause
                .first()
                .is_some_and(|&first| self.hunts.commitment_source(first).is_some());
        if commits {
            self.hunt_close();
        }
    }

    pub(crate) fn hunt_close(&mut self) {
        let located = self
            .hunts
            .active
            .as_ref()
            .filter(|active| active.located)
            .map(|active| active.start.failure);
        self.hunts.active = None;
        if let Some(source) = located {
            if self
                .hunts
                .located
                .get(&source)
                .is_some_and(|entry| entry.end == LocatedEnd::Consumed)
            {
                self.hunts.located.remove(&source);
            }
        }
    }

    pub(crate) fn hunt_phase_reset(&mut self, core: CoreId) {
        if self.core(core).queued_reset.is_none() {
            return;
        }
        let drops = self
            .hunts
            .active
            .as_ref()
            .and_then(|active| active.end.as_ref())
            .is_some_and(|end| end.payload.cores.contains(core));
        if drops {
            self.hunt_close();
        }
    }

    pub(crate) fn hunt_reset(&mut self, seq: Seq, core: CoreId) {
        self.hunts.reset_core(seq, core);
        if self
            .hunts
            .rejecting
            .as_ref()
            .is_some_and(|rejecting| rejecting.open.pending.class.loaded().contains(core))
        {
            self.hunts.rejecting = None;
        }
    }

    pub(crate) fn hunt_passed_full_cycle(&mut self, seq: Seq) {
        if let Some(profile) = self.checking.profile() {
            self.hunts.passed_full_cycle(seq, profile);
        }
    }

    pub(crate) fn hunt_note_parked_failure(
        &mut self,
        seq: Seq,
        class: ClassKey,
        failure: &togi_journal::Failure,
        carried: bool,
    ) {
        use togi_journal::Attribution;
        use togi_machine::Condition;
        if failure.condition != Some(Condition::Parked)
            || failure.attribution != Attribution::Attributed
            || failure.known_failure.is_some()
        {
            return;
        }
        if let (Some(core), Some(offset)) = (failure.core, failure.offset) {
            self.hunts.note_parked_failure(
                class,
                ParkedFailure {
                    seq,
                    core,
                    offset,
                    carried,
                },
            );
        }
    }
}

impl Run {
    pub(crate) fn hunt_keep_rejecting(
        &mut self,
        ledger: Seq,
        cited: Seq,
        pending: &crate::failures::Pending,
        trial: Option<&crate::r7::failed::FailedTrial>,
    ) {
        use togi_machine::Condition;
        let group = pending.failure.condition == Some(Condition::Parked)
            && pending.failure.trial.is_some()
            && !pending.carried;
        let (true, true, Some(trial)) = (self.hunts.active_located(), group, trial) else {
            return;
        };
        self.hunts.rejecting = Some(Box::new(super::state::Rejecting {
            ledger,
            open: crate::r7::failed::OpenFailure {
                seq: cited,
                pending: pending.clone(),
                r7: trial.clone(),
                handled: CoreSet::default(),
            },
        }));
    }

    pub(crate) fn hunt_located_culprit(
        &mut self,
        seq: Seq,
        class: ClassKey,
        failure: &togi_journal::Failure,
    ) {
        use togi_journal::Attribution;
        use togi_machine::Condition;
        let (Some(core), Some(offset)) = (failure.core, failure.offset) else {
            return;
        };
        let culprit = failure.condition == Some(Condition::Parked)
            && failure.attribution == Attribution::Attributed
            && offset != TargetOffset::ZERO
            && !class.loaded().contains(core);
        if culprit {
            self.update_core(core, |state| state.pending = Some(seq));
        }
    }
}
