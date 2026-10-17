use std::fmt::Write as _;
use std::num::NonZeroU32;

use togi_journal::{
    Activity, Combination, DeadEnd, DeadEndCondition, Decision, HuntEnd, HuntGroup, HuntOutcome,
    HuntSkipped, HuntStage, HuntStart, Member, Seq, TunerDecision,
};
use togi_machine::{Condition, CoreSet, PerCore, TargetOffset};

use super::plan::{
    Backoff, Duration, End, Found, Group, GroupOutcome, GroupStage, MemberProbes, Probe,
    ProbeResult, ProbeStep, already_broken, combination_backoff, group_profile, park, park_located,
};
use super::state::{Active, GroupRecord};
use crate::failures::Pending;
use crate::ledger::carried_clause;
use crate::r7::respond::R7Route;
use crate::state::Run;
use crate::{
    Action, ClassKey, DecisionAction, Depth, Load, Profile, Requirement, Secs, TrialContext,
    TrialPlan, Window, WorkloadChoice,
};

enum Settled {
    Closing(DecisionAction),
    Plan(DecisionAction),
}

pub(super) enum Next {
    Group(Box<Group>),
    End(End),
}

fn member_list(profile: &Profile, cores: CoreSet) -> Vec<Member> {
    profile
        .among(cores)
        .map(|(core, offset)| Member { core, offset })
        .collect()
}

fn offsets_text(members: &[(togi_machine::CoreId, TargetOffset)]) -> String {
    let parts: Vec<String> = members
        .iter()
        .map(|(core, offset)| format!("core {:02} {}", core.get(), offset.get()))
        .collect();
    parts.join(" + ")
}

pub(super) const fn stage_of(stage: GroupStage) -> HuntStage {
    match stage {
        GroupStage::Locate => HuntStage::Locate,
        GroupStage::Part | GroupStage::Repeated => HuntStage::Part,
        GroupStage::Complement => HuntStage::Complement,
        GroupStage::Full => HuntStage::Full,
    }
}

impl Run {
    pub(super) fn hunt_secs(hunt: &Active, duration: Duration) -> Secs {
        match duration {
            Duration::Short => hunt.short,
            Duration::Failed => hunt.class.duration(),
        }
    }

    pub(crate) fn hunt_requirement(&self, hunt: &Active, group: &GroupRecord) -> Requirement {
        Requirement {
            context: TrialContext::Hunt {
                hunt: hunt.id,
                group: group.payload.group,
            },
            class: group.class,
            window: Window::LiveOrCarried {
                since: self.hunt_since(group.payload.stage, group.seq),
            },
            needed: hunt.needed,
        }
    }

    fn hunt_trial(&self, hunt: &Active, group: &GroupRecord) -> TrialPlan {
        let requirement = self.hunt_requirement(hunt, group);
        if let Some(retry) = self.retry.as_ref().filter(|retry| {
            retry.requirement() == requirement && self.retry_requirement(retry) == Some(requirement)
        }) {
            let mut plan = retry.plan.clone();
            plan.cause = vec![group.seq];
            return plan;
        }
        TrialPlan {
            regime: hunt.class.regime(),
            workload: WorkloadChoice::Frozen(hunt.class.workload()),
            load: Load::Many(hunt.class.loaded()),
            condition: Condition::Parked,
            activity: Activity::Hunt,
            profile: group.profile,
            duration: group.class.duration(),
            requirement,
            retry: false,
            cause: vec![group.seq],
        }
    }

    pub(super) fn plan_next(&self, hunt: &Active) -> Next {
        if hunt.located {
            if let Some(locate) = hunt.groups.first() {
                if matches!(
                    self.answer_of(hunt, locate).outcome,
                    Some(GroupOutcome::Failed | GroupOutcome::Skipped)
                ) {
                    return Next::End(End::Loaded);
                }
            }
        }
        let mut plan = hunt.plan.clone();
        let split = hunt.first_probe();
        if let Some(last) = split.checked_sub(1).and_then(|at| hunt.groups.get(at)) {
            let outcome = self.answer_of(hunt, last).outcome;
            plan.advance(outcome.unwrap_or(GroupOutcome::Skipped));
        }
        if let Some(end) = plan.end() {
            return Next::End(end);
        }
        match plan.next_group() {
            Some(group) => Next::Group(Box::new(group)),
            None => Next::End(End::Loaded),
        }
    }

    fn group_payload(hunt: &Active, group: &Group, reason: &str) -> HuntGroup {
        let cores = group.cores;
        HuntGroup {
            hunt: hunt.id,
            group: Self::next_group_number(hunt),
            cores,
            profile: group_profile(&hunt.parked, &hunt.failing, cores).offsets(),
            set: group.set,
            granularity: u32::try_from(group.granularity).unwrap_or(u32::MAX),
            stage: stage_of(group.stage),
            index: u32::try_from(group.index).unwrap_or(u32::MAX),
            duration_s: Self::hunt_secs(hunt, group.duration).get(),
            escalated: group.escalated,
            full_checked: group.full_checked,
            any_failed: group.any_failed,
            probe: None,
            held: Vec::new(),
            inferred: None,
            skipped: false,
            reason: Some(reason.to_owned()),
        }
    }

    fn next_group_number(hunt: &Active) -> NonZeroU32 {
        let next = u32::try_from(hunt.groups.len() + 1).unwrap_or(u32::MAX);
        NonZeroU32::new(next).unwrap_or(NonZeroU32::MIN)
    }

    fn planned(
        &self,
        hunt: &Active,
        mut payload: HuntGroup,
        note: &[String],
        priority: &[Seq],
        repeated: &[Seq],
    ) -> DecisionAction {
        let class = hunt.class_at(Secs::new(
            NonZeroU32::new(payload.duration_s).unwrap_or(NonZeroU32::MIN),
        ));
        let profile = Profile::new(&self.topo, payload.profile).unwrap_or(hunt.failing);
        let since = match payload.stage {
            HuntStage::Part | HuntStage::Complement => self.hunts.reset,
            HuntStage::Full | HuntStage::Locate | HuntStage::Probe => Some(hunt.seq),
        };
        let mut cause = vec![hunt.seq];
        let answer = self.evidence_answer(hunt, class, &profile, since);
        let mut reason = payload.reason.take().unwrap_or_default();
        match answer.outcome {
            Some(GroupOutcome::Passed) => {
                payload.inferred = Some(togi_journal::Inference::Pass);
                "passing trials already establish the group".clone_into(&mut reason);
                self.cite_carried(&mut cause, answer.carried.iter().copied());
            }
            Some(GroupOutcome::Failed) => {
                payload.inferred = Some(togi_journal::Inference::Failure);
                "a known failure establishes the group".clone_into(&mut reason);
                if let Some(failure) = answer.failure {
                    Self::cite_new(&mut cause, failure);
                }
            }
            Some(GroupOutcome::Skipped) | None => {
                if let Ok(Some(reached)) = self.constraints.reaches(&profile) {
                    payload.skipped = true;
                    reason = format!("its profile reaches {reached}");
                }
            }
        }
        if !priority.is_empty() {
            reason = format!(
                "{reason}; {} valid short trials preceded the longer failure, with no failure in this regime at the short duration or less since reset, so test the failed duration",
                priority.len()
            );
            cause.extend(priority.iter().copied());
        }
        cause.extend(repeated.iter().copied());
        for text in note {
            reason.push_str(text);
        }
        for index in 0..hunt.groups.len() {
            self.cite_group(hunt, index, &mut cause);
        }
        reason.push_str(&carried_clause(&self.evidence, &cause));
        payload.reason = Some(reason);
        DecisionAction::new(payload, cause)
    }

    fn plan_group(&self, hunt: &Active, group: &Group) -> DecisionAction {
        let located_full = hunt.located
            && group.stage == GroupStage::Full
            && Self::hunt_secs(hunt, group.duration) == hunt.class.duration();
        let reason = match group.stage {
            GroupStage::Locate => {
                "rerunning the failed load with every unloaded core at CO 0 to locate the failure"
            }
            GroupStage::Full if located_full => {
                "rerunning the full failing profile at the failed duration before the failure stays with the loaded cores"
            }
            GroupStage::Part | GroupStage::Complement | GroupStage::Full | GroupStage::Repeated => {
                "testing the part of the failing profile"
            }
        };
        let payload = Self::group_payload(hunt, group, reason);
        let first_narrowing = hunt.groups.len() == usize::from(hunt.located);
        let priority: &[Seq] = if first_narrowing && group.stage != GroupStage::Locate {
            &hunt.prior
        } else {
            &[]
        };
        let mut note = Vec::new();
        let mut repeated_seqs = Vec::new();
        if let (GroupStage::Repeated, Some(repeated)) = (group.stage, hunt.repeated) {
            note.push(format!(
                "; two parked failures in this class followed one-count backoffs on core {:02}, so probe that core first",
                repeated.core.get()
            ));
            repeated_seqs.extend(repeated.failures);
        }
        self.planned(hunt, payload, &note, priority, &repeated_seqs)
    }

    fn probe_results(&self, hunt: &Active) -> Vec<ProbeResult> {
        hunt.groups
            .iter()
            .filter_map(|group| {
                let probe = group.payload.probe.as_ref()?;
                let outcome = self
                    .answer_of(hunt, group)
                    .outcome
                    .filter(|outcome| *outcome != GroupOutcome::Skipped)
                    .unwrap_or(GroupOutcome::Skipped);
                Some(ProbeResult {
                    member: probe.core,
                    offset: probe.offset,
                    outcome,
                })
            })
            .collect()
    }

    fn probe_group(&self, hunt: &Active, probe: &Probe, reason: String) -> DecisionAction {
        let plan = &hunt.plan;
        let held: Vec<Member> = probe
            .held
            .iter()
            .map(|(core, offset)| Member { core, offset })
            .collect();
        let cores: CoreSet = held.iter().map(|member| member.core).collect();
        let payload = HuntGroup {
            hunt: hunt.id,
            group: Self::next_group_number(hunt),
            cores,
            profile: probe.profile(&hunt.parked).offsets(),
            set: if probe.held.is_empty() {
                std::iter::once(probe.member).collect()
            } else {
                plan.set()
            },
            granularity: u32::try_from(plan.granularity()).unwrap_or(u32::MAX),
            stage: HuntStage::Probe,
            index: u32::try_from(probe.index).unwrap_or(u32::MAX),
            duration_s: Self::hunt_secs(hunt, plan.duration()).get(),
            escalated: plan.escalated(),
            full_checked: plan.full_checked(),
            any_failed: true,
            probe: Some(Member {
                core: probe.member,
                offset: probe.offset,
            }),
            held,
            inferred: None,
            skipped: false,
            reason: Some("testing the probe profile".to_owned()),
        };
        self.planned(hunt, payload, &[reason], &[], &[])
    }

    pub(super) fn singleton_probing(
        &self,
        hunt: &Active,
        core: togi_machine::CoreId,
    ) -> Option<ProbeStep> {
        let group = hunt.groups.get(hunt.first_probe().checked_sub(1)?)?;
        let singleton = std::iter::once(core).collect();
        let (Some(failing), Some(anchor)) = (hunt.failing.get(core), hunt.parked.get(core)) else {
            return None;
        };
        if group.payload.cores != singleton
            || group.payload.inferred.is_some()
            || group.profile != group_profile(&hunt.parked, &hunt.failing, singleton)
            || self.answer_of(hunt, group).outcome != Some(GroupOutcome::Failed)
            || self.core(core).offset.shallower_than(failing)
            || anchor.get() - failing.get() <= 1
        {
            return None;
        }
        let mut probes = self.probe_results(hunt);
        let stopped = || {
            ProbeStep::Done(Found {
                members: std::iter::once((core, failing)).collect(),
                stopped: true,
            })
        };
        if probes.iter().any(|probe| {
            probe.member == core && probe.offset == anchor && probe.outcome != GroupOutcome::Passed
        }) {
            return Some(stopped());
        }
        let answer = self.evidence_answer(hunt, group.class, &hunt.parked, Some(hunt.seq));
        if answer.outcome != Some(GroupOutcome::Passed) {
            return Some(ProbeStep::Run(Probe {
                member: core,
                offset: anchor,
                held: PerCore::default(),
                index: probes.len(),
            }));
        }
        let index = probes.len();
        probes.retain(|probe| probe.offset != anchor);
        let mut step = MemberProbes::new(singleton, &hunt.failing, &hunt.parked).step(&probes);
        if let ProbeStep::Run(probe) = &mut step {
            probe.index = index;
        }
        Some(step)
    }

    fn hunt_end_action(&self, hunt: &Active, end: End) -> Settled {
        let loaded = hunt.class.loaded();
        let (result, mut reason, cores, members) = match end {
            End::Loaded => {
                let locate_failed = hunt.located
                    && hunt.groups.first().is_some_and(|locate| {
                        !matches!(
                            self.answer_of(hunt, locate).outcome,
                            Some(GroupOutcome::Passed)
                        )
                    });
                let reason = if locate_failed {
                    "the failed load failed again with every unloaded core at CO 0, so the failure stays with the loaded cores"
                } else {
                    "the full failing profile passed while narrowing the unloaded cores, so the failure stays with the loaded cores"
                };
                (HuntOutcome::Loaded, reason.to_owned(), loaded, Vec::new())
            }
            End::Culprit(core) => {
                let mut reason =
                    "parked trial outcomes identified the minimal failing set".to_owned();
                let mut members = Vec::new();
                if let Some(step) = self.singleton_probing(hunt, core) {
                    match step {
                        ProbeStep::Run(probe) => {
                            let reason = match hunt.parked.get(core) {
                                Some(anchor) if anchor != probe.offset => format!(
                                    "; probing core {:02} between its reproduced singleton failure and the passing parked anchor {}",
                                    core.get(),
                                    anchor.get()
                                ),
                                Some(_) | None => format!(
                                    "; testing core {:02} at parked offset {} to establish a safe singleton anchor; CO 0 is not assumed to pass",
                                    core.get(),
                                    probe.offset.get()
                                ),
                            };
                            return Settled::Plan(self.probe_group(hunt, &probe, reason));
                        }
                        ProbeStep::Done(found) => {
                            if let Some(point) =
                                found.members.get(core).or_else(|| hunt.failing.get(core))
                            {
                                members.push(Member { core, offset: point });
                                if found.stopped {
                                    reason.push_str(
                                        "; singleton probing stopped without a safe bracket; retain the shallowest reproduced failure",
                                    );
                                } else {
                                    let _ = write!(
                                        reason,
                                        "; singleton probes found failure at {} and a passing anchor or probe one count shallower",
                                        point.get()
                                    );
                                }
                            }
                        }
                    }
                }
                (
                    HuntOutcome::Culprit,
                    reason,
                    std::iter::once(core).collect(),
                    members,
                )
            }
            End::Fallback(set) => (
                HuntOutcome::Fallback,
                "no tested group failed, so the remaining candidates stay unresolved and form a combination".to_owned(),
                set,
                Vec::new(),
            ),
            End::Combination(set) => {
                let base = "parked trial outcomes identified the minimal failing set";
                let failing_members = member_list(&hunt.failing, set);
                let broken = self.checking.profile().is_some_and(|profile| {
                    failing_members
                        .iter()
                        .any(|member| {
                    profile
                        .get(member.core)
                        .is_some_and(|at| at.shallower_than(member.offset))
                })
                });
                if broken {
                    (HuntOutcome::Combination, base.to_owned(), set, Vec::new())
                } else {
                    let probes = MemberProbes::new(set, &hunt.failing, &hunt.parked);
                    match probes.step(&self.probe_results(hunt)) {
                        ProbeStep::Run(probe) => {
                            return Settled::Plan(self.probe_group(
                                hunt,
                                &probe,
                                format!(
                                    "; probing how shallow core {:02} must be for the combination to pass",
                                    probe.member.get()
                                ),
                            ));
                        }
                        ProbeStep::Done(found) => {
                            let listed: Vec<_> = found.members.iter().collect();
                            let clause = if found.stopped {
                                format!(
                                    "; member probes stopped at a skipped group with it failing at {}",
                                    offsets_text(&listed)
                                )
                            } else {
                                format!(
                                    "; member probes found it still failing at {} and passing with any one member a count shallower",
                                    offsets_text(&listed)
                                )
                            };
                            let members = listed
                                .iter()
                                .map(|&(core, offset)| Member { core, offset })
                                .collect();
                            (
                                HuntOutcome::Combination,
                                format!("{base}{clause}"),
                                set,
                                members,
                            )
                        }
                    }
                }
            }
        };
        let mut cause = vec![hunt.seq];
        if result == HuntOutcome::Loaded {
            if let Some(failure) = self.rejecting_failure(hunt) {
                Self::cite_new(&mut cause, failure);
            }
        }
        for index in 0..hunt.groups.len() {
            if result == HuntOutcome::Culprit && !members.is_empty() {
                Self::cite_new(&mut cause, hunt.groups[index].seq);
            }
            self.cite_group(hunt, index, &mut cause);
        }
        reason.push_str(&carried_clause(&self.evidence, &cause));
        Settled::Closing(DecisionAction::new(
            HuntEnd {
                hunt: hunt.id,
                result,
                cores,
                members,
                groups: u32::try_from(hunt.groups.len()).unwrap_or(u32::MAX),
                reason,
            },
            cause,
        ))
    }

    fn located_loaded_failure(&self, hunt: &Active) -> Option<DecisionAction> {
        if !hunt.located {
            return None;
        }
        let failure = self.rejecting_failure(hunt)?;
        if hunt.groups.len() > 1 && Self::loaded_all_zero(hunt) {
            return None;
        }
        let named = self
            .failures
            .witnessed(failure)?
            .failure
            .core
            .filter(|&core| hunt.class.loaded().contains(core))?;
        let reason = format!(
            "failure #{failure} named loaded core {:02}, so the failure stays with the loaded cores and core {:02} is charged for it",
            named.get(),
            named.get()
        );
        Some(DecisionAction::new(
            HuntEnd {
                hunt: hunt.id,
                result: HuntOutcome::Loaded,
                cores: hunt.class.loaded(),
                members: Vec::new(),
                groups: u32::try_from(hunt.groups.len()).unwrap_or(u32::MAX),
                reason,
            },
            vec![hunt.seq, failure],
        ))
    }

    fn settle(&self, hunt: &Active) -> Settled {
        if let Some(group) = hunt.latest().filter(|group| {
            group.payload.set.len() == 1 && group.payload.probe.is_some() && group.profile.is_zero()
        }) {
            if let Some(failure) = self.answer_of(hunt, group).failure {
                return Settled::Closing(DecisionAction::new(
                    DeadEnd {
                        condition: DeadEndCondition::FailureAtZero,
                        core: None,
                        detail: "the singleton anchor failed with every core at CO 0; the instability is not caused by Curve Optimizer".to_owned(),
                        action: None,
                    },
                    vec![hunt.seq, group.seq, failure],
                ));
            }
        }
        if let Some(end) = self.located_loaded_failure(hunt) {
            return Settled::Closing(end);
        }
        match self.plan_next(hunt) {
            Next::Group(group) => Settled::Plan(self.plan_group(hunt, &group)),
            Next::End(end) => self.hunt_end_action(hunt, end),
        }
    }

    pub(crate) fn hunt_settled(&self) -> bool {
        self.hunts.active.as_ref().is_some_and(|hunt| {
            hunt.end.is_some()
                || (hunt.latest().is_some() && self.unanswered_latest(hunt).is_none())
        })
    }

    fn unanswered_latest<'a>(&self, hunt: &'a Active) -> Option<&'a GroupRecord> {
        hunt.latest()
            .filter(|latest| self.answer_of(hunt, latest).outcome.is_none())
    }

    /// The decision that closes the hunt: its end, then its combination record and backoff. Never
    /// a group plan or a trial, so it is `None` while a group is unanswered or still to plan.
    pub(crate) fn hunt_closing(&self) -> Option<DecisionAction> {
        let hunt = self.hunts.active.as_ref()?;
        if hunt.end.is_some() {
            return self.hunt_commitment(hunt);
        }
        if self.unanswered_latest(hunt).is_some() {
            return None;
        }
        match self.settle(hunt) {
            Settled::Closing(decision) => Some(decision),
            Settled::Plan(_) => None,
        }
    }

    pub(crate) fn hunt_next(&self) -> Option<Action> {
        let hunt = self.hunts.active.as_ref()?;
        if hunt.end.is_some() {
            return self.hunt_commitment(hunt).map(Action::Decide);
        }
        if let Some(latest) = self.unanswered_latest(hunt) {
            if let Ok(Some(reached)) = self.constraints.reaches(&latest.profile) {
                let mut payload = latest.payload.clone();
                payload.skipped = true;
                payload.inferred = None;
                payload.reason = Some(format!("its profile reaches {reached}"));
                return Some(Action::Decide(DecisionAction::new(
                    payload,
                    vec![latest.seq],
                )));
            }
            return Some(Action::RunTrial(self.hunt_trial(hunt, latest)));
        }
        Some(Action::Decide(match self.settle(hunt) {
            Settled::Closing(decision) | Settled::Plan(decision) => decision,
        }))
    }

    pub(crate) fn hunt_commitment(&self, hunt: &Active) -> Option<DecisionAction> {
        let end = hunt.end.as_ref()?;
        if end.payload.result == HuntOutcome::Direct {
            return None;
        }
        if end.payload.cores.len() == 1 {
            return self.culprit_backoff(hunt, end.seq, end.payload.cores.first()?);
        }
        match &end.combination {
            None => Some(self.combination_record(hunt)),
            Some(recorded) => self.combination_commitment(hunt, recorded),
        }
    }

    fn culprit_backoff(
        &self,
        hunt: &Active,
        end: Seq,
        id: togi_machine::CoreId,
    ) -> Option<DecisionAction> {
        let core = self.core(id);
        let failing = hunt
            .end
            .as_ref()
            .and_then(|end| end.payload.members.iter().find(|member| member.core == id))
            .map(|member| member.offset)
            .or_else(|| hunt.failing.get(id))?;
        let point = self
            .failure_point(id)
            .filter(|&fail| fail.shallower_than(failing))
            .unwrap_or(failing);
        let past = point.shallower_by(1);
        let to = if core.offset.shallower_than(past) {
            core.offset
        } else {
            past
        };
        let (pass, discarded) = match core.pass {
            Some(pass) if !pass.shallower_than(point) => (
                None,
                format!(
                    "; passed step at {} discarded, the failure contradicts it",
                    pass.get()
                ),
            ),
            kept => (kept, String::new()),
        };
        Some(DecisionAction::new(
            TunerDecision {
                core: id,
                activity: Activity::Hunt,
                decision: Decision::Backoff,
                from_offset: core.offset,
                to_offset: to,
                pass,
                failure_point: Some(point),
                workloads: Vec::new(),
                reason: format!(
                    "hunt {} found core {:02}; failure point {}{discarded}{}",
                    hunt.id,
                    id.get(),
                    point.get(),
                    hunt.paired_clause()
                ),
            },
            vec![end],
        ))
    }

    fn combination_record(&self, hunt: &Active) -> DecisionAction {
        let Some(end) = hunt.end.as_ref() else {
            return DecisionAction::new(
                HuntSkipped {
                    failure: hunt.start.failure,
                    reason: String::new(),
                },
                Vec::new(),
            );
        };
        let members = if end.payload.members.is_empty() {
            member_list(&hunt.failing, end.payload.cores)
        } else {
            end.payload.members.clone()
        };
        let mut reason = format!("hunt {} found a combined failure", hunt.id);
        if let Some(profile) = self.checking.profile() {
            if let Some(member) = members.iter().find(|member| {
                profile
                    .get(member.core)
                    .is_some_and(|at| at.shallower_than(member.offset))
            }) {
                reason = format!(
                    "no backoff: core {:02} is already shallower",
                    member.core.get()
                );
            }
        }
        let id = NonZeroU32::new(self.hunts.next_combination + 1).unwrap_or(NonZeroU32::MIN);
        DecisionAction::new(
            Combination {
                combination: id,
                members,
                fallback: end.payload.result == HuntOutcome::Fallback,
                hunt: hunt.id,
                reason: reason + &hunt.paired_clause(),
            },
            vec![end.seq],
        )
    }

    pub(crate) fn combination_commitment(
        &self,
        hunt: &Active,
        recorded: &super::state::Recorded,
    ) -> Option<DecisionAction> {
        let current = self.offsets();
        if self
            .checking
            .profile()
            .is_some_and(|profile| already_broken(&recorded.members, &profile))
        {
            return None;
        }
        let mut passed: Vec<Probe> = Vec::new();
        let mut cited: Vec<(Seq, NonZeroU32)> = Vec::new();
        for group in &hunt.groups {
            let Some(probe) = group.payload.probe.as_ref() else {
                continue;
            };
            if self.answer_of(hunt, group).outcome != Some(GroupOutcome::Passed) {
                continue;
            }
            passed.push(Probe {
                member: probe.core,
                offset: probe.offset,
                held: group
                    .payload
                    .held
                    .iter()
                    .map(|member| (member.core, member.offset))
                    .collect(),
                index: passed.len(),
            });
            cited.push((group.seq, group.payload.group));
        }
        let backoff = combination_backoff(
            &recorded.members,
            &passed,
            &hunt.failing,
            &current,
            &self.constraints,
            &hunt.ranking,
        )?;
        let (member, to, cause, reason) = match backoff {
            Backoff::Tested { probe, member, to } => {
                let (seq, group) = *cited.get(probe)?;
                (
                    member,
                    to,
                    vec![recorded.seq, seq],
                    format!(
                        "combination {}: backing off core {:02} to {}, where hunt {} group {group} passed with the rest of the combination at its failing offsets{}",
                        recorded.id,
                        member.get(),
                        to.get(),
                        hunt.id,
                        hunt.paired_clause()
                    ),
                )
            }
            Backoff::Untested { member, to, reach } => (
                member,
                to,
                vec![recorded.seq],
                format!(
                    "combination {}: backing off core {:02} leaves {reach} counts reachable{}",
                    recorded.id,
                    member.get(),
                    hunt.paired_clause()
                ),
            ),
        };
        let core = self.core(member);
        Some(DecisionAction::new(
            TunerDecision {
                core: member,
                activity: Activity::Hunt,
                decision: Decision::Backoff,
                from_offset: core.offset,
                to_offset: to,
                pass: core.pass,
                failure_point: self.failure_point(member),
                workloads: Vec::new(),
                reason,
            },
            cause,
        ))
    }

    pub(crate) fn hunt_direct(
        &self,
        id: togi_machine::CoreId,
        seq: Seq,
        record: &Pending,
    ) -> Option<DecisionAction> {
        let hunt = self
            .hunts
            .active
            .as_ref()
            .filter(|hunt| hunt.end.is_none())?;
        (record.failure.condition == Some(Condition::Parked)).then(|| {
            DecisionAction::new(
                HuntEnd {
                    hunt: hunt.id,
                    result: HuntOutcome::Direct,
                    cores: std::iter::once(id).collect(),
                    members: Vec::new(),
                    groups: u32::try_from(hunt.groups.len()).unwrap_or(u32::MAX),
                    reason: format!("attributed failure #{seq}"),
                },
                vec![seq],
            )
        })
    }

    /// The direct end of the open hunt and its paired clause: the ordinary backoff that answers
    /// the direct failure cites the end.
    pub(crate) fn hunt_direct_end(&self) -> Option<(Seq, String)> {
        let hunt = self.hunts.active.as_ref()?;
        let end = hunt.end.as_ref()?;
        (end.payload.result == HuntOutcome::Direct).then(|| (end.seq, hunt.paired_clause()))
    }

    pub(crate) fn hunt_cancel(
        &self,
        core: togi_machine::CoreId,
        queued: Seq,
    ) -> Option<DecisionAction> {
        let hunt = self
            .hunts
            .active
            .as_ref()
            .filter(|hunt| hunt.end.is_none())?;
        Some(DecisionAction::new(
            HuntEnd {
                hunt: hunt.id,
                result: HuntOutcome::Cancelled,
                cores: CoreSet::default(),
                members: Vec::new(),
                groups: u32::try_from(hunt.groups.len()).unwrap_or(u32::MAX),
                reason: format!("core {:02} was reset", core.get()),
            },
            vec![queued],
        ))
    }

    /// The decision for the failure at the head of the hunt queue: skipped when its profile
    /// reaches a recorded constraint, held when it is a first later failure, else its hunt,
    /// paired with the held failure it is the second of.
    pub(crate) fn hunt_start_action(&self) -> Option<Action> {
        let seq = self.failures.queue().next()?;
        let record = self.failures.pending(seq)?;
        if self.hunts.active.is_some() {
            return None;
        }
        if let Ok(Some(reached)) = self.constraints.reaches(&record.profile) {
            return Some(Action::Decide(DecisionAction::new(
                HuntSkipped {
                    failure: seq,
                    reason: format!("its profile reaches {reached}"),
                },
                vec![seq],
            )));
        }
        if let Some(hold) = self.later_hunt_hold() {
            return Some(Action::Decide(hold));
        }
        if self.ranking_stale() {
            return Some(Action::ReadRanking);
        }
        let class = record.class;
        let parked = park(&record.profile, self.hunts.passed_newest_first());
        let mut cause = vec![seq];
        cause.extend(parked.source);
        let mut reason = record.failure.reason.clone();
        if let Some((strike, clause)) = self.later_hunt_pair(seq) {
            cause.push(strike);
            reason = Some(match reason {
                Some(known) if !known.is_empty() => format!("{known}; {clause}"),
                _ => clause,
            });
        }
        let id = NonZeroU32::new(self.hunts.next_id + 1).unwrap_or(NonZeroU32::MIN);
        Some(Action::Decide(DecisionAction::new(
            HuntStart {
                hunt: id,
                failure: seq,
                trial: record.failure.trial.clone(),
                regime: class.regime(),
                workload: class.workload(),
                cores: class.loaded(),
                duration_s: class.duration().get(),
                failing: record.profile.offsets(),
                parked: parked.profile.offsets(),
                parked_seq: parked.source,
                candidates: parked.candidates,
                trials: u32::from(self.config.needed.get()),
                trial_s: self.config.short_trial.get(),
                miss: self.config.miss,
                rate: self.config.rate,
                ranking: self
                    .ranking
                    .as_ref()
                    .map(|ranked| ranked.ranking.order().to_vec())
                    .unwrap_or_default(),
                reason,
            },
            cause,
        )))
    }

    pub(crate) fn locate_action(&self) -> Option<Action> {
        if self.hunts.active.is_some() {
            return None;
        }
        let R7Route::Locate { failure, reason } = self.locate_due()? else {
            return None;
        };
        if let Some(close) = self.round_locate_close(failure) {
            return Some(Action::Decide(close));
        }
        if self.ranking_stale() {
            return Some(Action::ReadRanking);
        }
        let record = self.r7.open.get(&failure)?;
        let pending = &record.pending;
        let class: ClassKey = pending.class;
        let parked = park_located(&pending.profile, class.loaded());
        let mut cause = vec![failure];
        cause.extend(reason.cause());
        let id = NonZeroU32::new(self.hunts.next_id + 1).unwrap_or(NonZeroU32::MIN);
        Some(Action::Decide(DecisionAction::new(
            HuntStart {
                hunt: id,
                failure,
                trial: pending.failure.trial.clone(),
                regime: class.regime(),
                workload: class.workload(),
                cores: class.loaded(),
                duration_s: class.duration().get(),
                failing: pending.profile.offsets(),
                parked: parked.profile.offsets(),
                parked_seq: None,
                candidates: parked.candidates,
                trials: u32::from(self.config.needed.get()),
                trial_s: self.config.short_trial.get(),
                miss: self.config.miss,
                rate: self.config.rate,
                ranking: self
                    .ranking
                    .as_ref()
                    .map(|ranked| ranked.ranking.order().to_vec())
                    .unwrap_or_default(),
                reason: Some(reason.to_string()),
            },
            cause,
        )))
    }

    pub(crate) fn skip_hunt_group(
        &self,
        plan: &TrialPlan,
        known: Seq,
        reason: String,
    ) -> Option<DecisionAction> {
        let TrialContext::Hunt { hunt, group } = plan.requirement.context else {
            return None;
        };
        let active = self
            .hunts
            .active
            .as_ref()
            .filter(|active| active.id == hunt)?;
        let latest = active
            .latest()
            .filter(|latest| latest.payload.group == group)?;
        let mut payload = latest.payload.clone();
        payload.inferred = Some(togi_journal::Inference::Failure);
        payload.skipped = false;
        payload.reason = Some(reason);
        Some(DecisionAction::new(payload, vec![latest.seq, known]))
    }
}
