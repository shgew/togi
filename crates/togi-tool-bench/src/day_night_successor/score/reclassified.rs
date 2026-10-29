//! Ruleset 14 §5: restoring contradicted depth preserves stops and historical charges.

use togi_machine::PerCore;
use togi_simrun::day_night::successor::{ProfileVeto, RestorationHold};

use super::{
    Audit, Classification, Period, ProfileStep, ReclassifiedRestorationView, StepKind, suspect,
};

impl Audit<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "one linear audit of the linked restoration witnesses"
    )]
    pub(super) fn reclassified_restorations(&mut self) {
        let record = self.record;
        let tolerance = self.rules.day_tolerance;
        for (index, step) in record.steps.iter().enumerate() {
            let StepKind::ReclassifiedRestoration {
                retreat,
                event,
                reclassified_day,
                units,
                hold,
                qualification,
                consumed_before,
                consumed_after,
                crash_vetoes,
            } = &step.kind
            else {
                continue;
            };
            let violations = self.result.violations.len();
            let Some(before) = self.depths(&step.id, &step.before) else {
                continue;
            };
            let Some(after) = self.depths(&step.id, &step.after) else {
                continue;
            };
            let source = record
                .events
                .iter()
                .find(|item| item.policy_event.id == *event);
            if !source.is_some_and(|source| {
                let observed = &source.policy_event;
                observed.classification == Classification::NonCpu
                    && observed.reclassified_from.is_some_and(suspect)
                    && !observed.provisional
                    && source.period == Period::Day
                    && observed.day <= *reclassified_day + tolerance
                    && *reclassified_day <= step.day + tolerance
            }) {
                self.violate("reclassified-event", &step.id,
                    "the source event has no preceding non-CPU reclassification of a CPU-suspect event");
            }
            let reverted = record.steps[..index]
                .iter()
                .find(|item| item.id == *retreat);
            let matching = reverted.is_some_and(|reverted| {
                let StepKind::Retreat {
                    event: Some(cause),
                    units: reverted_units,
                } = &reverted.kind
                else {
                    return false;
                };
                *cause == *event
                    && units.len() == reverted_units.len()
                    && !units.is_empty()
                    && units.iter().all(|unit| reverted_units.contains(unit))
                    && source
                        .is_some_and(|source| reverted.day + tolerance >= source.policy_event.day)
                    && reverted.day <= *reclassified_day + tolerance
            });
            if !matching {
                self.violate("reclassified-retreat-units", &step.id,
                    "restored units do not exactly match a preceding retreat caused by the reclassified event");
            }
            if !reverted.is_some_and(|reverted| {
                let mut expected = step.before;
                let retreat_index = record.steps.iter().position(|item| item.id == reverted.id);
                if !retreat_index.is_some_and(|at| units.iter().all(|unit| {
                    record.steps[..at].iter().any(|applied| {
                        matches!(&applied.kind, StepKind::Climb { unit: climbed, .. } if climbed == unit)
                            && record.provenance.membership[unit.index()].cores.iter().all(|core| {
                                applied.after.get(*core) == reverted.before.get(*core)
                                    && applied.before.get(*core) < applied.after.get(*core)
                            })
                    })
                })) { return false; }
                for unit in units {
                    for core in &record.provenance.membership[unit.index()].cores {
                        let (Some(old), Some(lower), Some(current)) = (reverted.before.get(*core), reverted.after.get(*core), step.before.get(*core)) else { return false };
                        if old <= lower || current != lower { return false; }
                        expected.set(*core, old);
                    }
                }
                expected == step.after
            }) {
                self.violate("reclassified-depth", &step.id,
                    "restoration must restore exactly the contradicted units' pre-retreat depths and change no other unit");
            }
            self.reclassified_hold(index, step, hold.as_ref());
            if let Some(certificate) = qualification {
                self.check_qualification(&step.id, step, certificate, &step.after);
            } else {
                self.violate(
                    "reclassified-qualification",
                    &step.id,
                    "no exact-profile night certificate",
                );
            }
            let spent = self.charged_before(index);
            if *consumed_after < *consumed_before
                || *consumed_before != spent
                || *consumed_after != spent
                || source.is_some_and(|source| !source.policy_event.charged)
            {
                self.violate("reclassified-refund", &step.id,
                    "restoration must preserve the raw historical consumed count, including the reclassified event");
            }
            if spent >= u64::from(self.rules.criteria.allowance_limit) {
                self.violate(
                    "reclassified-after-exhaustion",
                    &step.id,
                    "no allowance remains for restoration",
                );
            }
            for unit in units {
                let stopped = record.policy.stopped_units.contains(unit)
                    && record.units[unit.index()]
                        .finish
                        .as_ref()
                        .is_some_and(|finish| {
                            source.is_some_and(|source| {
                                finish.day <= source.policy_event.day + tolerance
                            })
                        });
                let reopened = record.steps[index + 1..].iter().any(|later| {
                    matches!(&later.kind, StepKind::Climb { unit: climbed, .. } if climbed == unit)
                });
                if !stopped || reopened {
                    self.violate(
                        "reclassified-stop",
                        &step.id,
                        format!(
                            "unit {} must retain its original stop and never promote again",
                            unit.get()
                        ),
                    );
                }
            }
            self.reclassified_blockers(step, &after);
            self.reclassified_crashes(index, step, crash_vetoes);
            self.result
                .reclassified_restorations
                .push(ReclassifiedRestorationView {
                    id: step.id.clone(),
                    day: step.day,
                    retreat: retreat.clone(),
                    event: event.clone(),
                    units: units.clone(),
                    before_depths: before,
                    after_depths: after,
                    hold_hours: hold.as_ref().map(|hold| hold.clean_hours),
                    qualification: qualification
                        .as_ref()
                        .map(|certificate| certificate.id.clone()),
                    consumed_before: *consumed_before,
                    consumed_after: *consumed_after,
                    violations: self.result.violations.len() - violations,
                });
        }
    }

    fn reclassified_hold(
        &mut self,
        index: usize,
        step: &ProfileStep,
        hold: Option<&RestorationHold>,
    ) {
        let Some(hold) = hold else {
            self.violate(
                "reclassified-hold",
                &step.id,
                "no fresh normal hold witness",
            );
            return;
        };
        let tolerance = self.rules.day_tolerance;
        let needed = f64::from(self.rules.criteria.clean_hold_hours);
        let previous = self.record.steps[..index]
            .last()
            .map_or(1.0, |step| step.day);
        let start = self.clean_since(previous, step.day);
        let hours: f64 = self
            .record
            .intervals
            .iter()
            .filter(|interval| {
                interval.period == Period::Day
                    && interval.start_day >= hold.start_day - tolerance
                    && interval.end_day <= step.day + tolerance
                    && interval.profile == step.before
            })
            .map(|interval| interval.verified_daily_active_hours)
            .sum();
        if hold.profile != step.before
            || hold.start_day < start - tolerance
            || hold.start_day > step.day + tolerance
            || hold.witnesses.is_empty()
            || hold.clean_hours + tolerance < needed
            || hours + tolerance < needed
            || hold.clean_hours > hours + tolerance
        {
            self.violate(
                "reclassified-hold",
                &step.id,
                "restoration requires a fresh witnessed 50-hour hold at the exact current profile",
            );
        }
    }

    fn reclassified_blockers(&mut self, step: &ProfileStep, after: &[u8]) {
        let record = self.record;
        let tolerance = self.rules.day_tolerance;
        for summary in &record.units {
            let evidence = summary
                .ceiling_evidence
                .iter()
                .filter(|evidence| {
                    evidence.day <= step.day + tolerance
                        && record
                            .epochs
                            .iter()
                            .find(|epoch| epoch.id == evidence.epoch)
                            .is_some_and(|epoch| {
                                Some(epoch.context.as_str()) == self.context_at(step.day)
                            })
                })
                .min_by_key(|evidence| evidence.ceiling_depth);
            if after[summary.unit.index()] > 0
                && evidence
                    .is_none_or(|ceiling| after[summary.unit.index()] > ceiling.ceiling_depth)
            {
                self.violate(
                    "reclassified-ceiling",
                    &step.id,
                    format!(
                        "unit {} lacks a compatible permitting ceiling",
                        summary.unit.get()
                    ),
                );
            }
        }
        for event in &record.events {
            let observed = &event.policy_event;
            if event.period == Period::Day
                && observed.day <= step.day + tolerance
                && suspect(observed.classification)
                && !super::zero(&event.profile)
                && step
                    .after
                    .iter()
                    .all(|(core, depth)| event.profile.get(core).is_some_and(|veto| depth >= veto))
            {
                self.violate(
                    "reclassified-veto",
                    &step.id,
                    "the restored vector violates an active whole-profile veto",
                );
            }
            if observed.day <= step.day + tolerance
                && observed.classification == Classification::Unanswered
            {
                self.violate(
                    "reclassified-pending",
                    &step.id,
                    "a pending question blocks restoration",
                );
            }
        }
    }

    fn reclassified_crashes(&mut self, index: usize, step: &ProfileStep, vetoes: &[ProfileVeto]) {
        let record = self.record;
        let tolerance = self.rules.day_tolerance;
        let closing = record.steps[index + 1..]
            .iter()
            .position(|later| later.after != step.after)
            .map(|offset| index + 1 + offset);
        for crash in record.events.iter().filter(|event| {
            event.period == Period::Day
                && event.profile == step.after
                && event.policy_event.day >= step.day - tolerance
                && closing.is_none_or(|end| {
                    event.policy_event.day <= record.steps[end].day + tolerance
                        || matches!(&record.steps[end].kind, StepKind::Retreat { event: Some(cause), .. } if *cause == event.policy_event.id)
                })
                && (suspect(event.policy_event.classification) || event.policy_event.provisional
                    || event.policy_event.reclassified_from.is_some_and(suspect))
        }) {
            let observed = &crash.policy_event;
            if !observed.charged {
                self.violate(
                    "reclassified-crash-charge",
                    &observed.id,
                    "a CPU-suspect crash at the restored profile must consume allowance",
                );
            }
            if !vetoes
                .iter()
                .any(|veto| veto.event == observed.id && veto.profile == step.after)
            {
                self.violate(
                    "reclassified-crash-veto",
                    &observed.id,
                    "the crash must veto the whole restored vector",
                );
            }
            let expected = self.escalated_profile(index);
            let stepped = closing.is_some_and(|end| {
                let retreat = &record.steps[end];
                matches!(&retreat.kind, StepKind::Retreat { event: Some(event), units } if *event == observed.id
                    && record.provenance.membership.iter().all(|member| {
                        let changed = member.cores.iter().any(|core| step.after.get(*core) != expected.get(*core));
                        units.contains(&member.unit) == changed
                    }))
                    && retreat.day + tolerance >= observed.day && retreat.before == step.after && retreat.after == expected
            });
            if !stepped {
                self.violate("reclassified-crash-retreat", &observed.id,
                    "the restored-profile crash must take the ordinary escalating newest-step backstep");
            }
        }
    }

    /// Reconstruct the remaining per-unit application history, consuming each retreat cursor once.
    fn escalated_profile(&self, through: usize) -> PerCore<u8> {
        let mut histories: Vec<Vec<u8>> = vec![Vec::new(); self.record.units.len()];
        let mut result = self.record.steps[through].after;
        for step in &self.record.steps[..=through] {
            for member in &self.record.provenance.membership {
                let Some(core) = member.cores.first() else {
                    continue;
                };
                let (Some(before), Some(after)) = (step.before.get(*core), step.after.get(*core))
                else {
                    continue;
                };
                let history = &mut histories[member.unit.index()];
                if after > before {
                    history.push(before);
                }
                if after < before {
                    while history.last().is_some_and(|previous| *previous >= after) {
                        history.pop();
                    }
                }
            }
        }
        for member in &self.record.provenance.membership {
            if let Some(previous) = histories[member.unit.index()].last() {
                for core in &member.cores {
                    result.set(*core, *previous);
                }
            }
        }
        result
    }
}
