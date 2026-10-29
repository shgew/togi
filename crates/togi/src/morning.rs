//! A morning is identified from existing night plans and day starts, never a new journal marker.

use std::fmt::Write as _;

use serde::Serialize;
use togi_journal::{Event, NightId, Payload};
use togi_machine::{BootId, object_only};

object_only! {
    /// The first observed day boot after the latest night. Socket data, not a journal payload.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Report {
        pub boot: BootId,
        pub text: String,
    }
}

impl Report {
    /// Reopening observation in the same boot retains the report; a subsequent day boot does not.
    /// Only durable records count: no forecast, crash-tail extrapolation or notification receipt.
    #[must_use]
    pub fn of(events: &[Event]) -> Option<Self> {
        let plan_event = events
            .iter()
            .rev()
            .find(|event| matches!(event.payload, Payload::NightPlan(_)))?;
        let Payload::NightPlan(plan) = &plan_event.payload else {
            return None;
        };
        let night = plan.night.unwrap_or(NightId(plan_event.seq));
        let opening = events.iter().find(|event| event.seq == night.0)?;
        let first_day = events.iter().find(|event| {
            event.seq > plan_event.seq
                && event.boot != opening.boot
                && matches!(event.payload, Payload::DayBootStart(_))
        })?;
        let latest_day = events
            .iter()
            .rev()
            .find(|event| matches!(event.payload, Payload::DayBootStart(_)))?;
        if latest_day.boot != first_day.boot
            || events.iter().any(|event| {
                event.seq > opening.seq && matches!(event.payload, Payload::CommandReset(_))
            })
        {
            return None;
        }
        let ended = events
            .iter()
            .filter(|event| {
                event.seq >= opening.seq
                    && event.seq < first_day.seq
                    && matches!(event.payload, Payload::TrialEnd(_))
            })
            .count();
        let mut text = format!("Last night {night}: {ended} recorded trial ends.");
        for budget in &plan.budgets {
            let _ = write!(
                text,
                " {}: {}s observed task time.",
                budget.queue.as_str(),
                budget.consumed_s
            );
        }
        text.push_str(" No automatic nonzero change is made at this boot; day application is restricted to stock (CO 0). This report does not verify the current hardware readback or establish stability. See togi status --evidence for cap reasons, hours and coverage gaps.");
        Some(Self {
            boot: first_day.boot.clone(),
            text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use togi_journal::{Budget, DayBootStart, NightPlan, Queue, Seq};
    use togi_machine::{BiosContext, PerCore};

    fn event(seq: u64, boot: &str, payload: impl Into<Payload>) -> Event {
        Event {
            seq: Seq::new(seq).unwrap(),
            time: Timestamp::UNIX_EPOCH,
            boot: boot.to_owned().into(),
            mono: None,
            cause: Vec::new(),
            msg: String::new(),
            payload: payload.into(),
        }
    }

    fn night() -> Event {
        event(
            1,
            "night",
            NightPlan {
                night: None,
                budgets: vec![Budget {
                    queue: Queue::Mapping,
                    share_pct: 10,
                    consumed_s: 120,
                }],
                tasks: Vec::new(),
                unavailable: Vec::new(),
            },
        )
    }

    fn day(seq: u64, boot: &str) -> Event {
        event(
            seq,
            boot,
            DayBootStart {
                bios: BiosContext::default(),
                build: crate::report::current_build(),
                offsets: PerCore::default(),
                watchdog: None,
            },
        )
    }

    #[test]
    fn only_the_first_observed_day_boot_gets_a_report() {
        let mut events = vec![night()];
        assert_eq!(Report::of(&events), None);
        events.push(day(2, "morning"));
        let report = Report::of(&events).unwrap();
        assert_eq!(report.boot.as_str(), "morning");
        assert!(report.text.contains("mapping: 120s observed task time"));
        assert!(report.text.contains("restricted to stock (CO 0)"));
        events.push(day(3, "morning"));
        assert_eq!(Report::of(&events), Some(report));
        events.push(day(4, "later"));
        assert_eq!(Report::of(&events), None);
    }

    #[test]
    fn day_start_in_the_night_boot_does_not_invent_a_morning() {
        assert_eq!(Report::of(&[night(), day(2, "night")]), None);
    }

    #[test]
    fn a_crash_reboot_plan_uses_the_original_night_identity() {
        let continued = event(
            2,
            "crash-reboot",
            NightPlan {
                night: Some(NightId(Seq::FIRST)),
                budgets: Vec::new(),
                tasks: Vec::new(),
                unavailable: Vec::new(),
            },
        );
        let report = Report::of(&[night(), continued, day(3, "morning")]).unwrap();
        assert_eq!(report.boot.as_str(), "morning");
        assert!(report.text.starts_with("Last night #1:"));
    }

    #[test]
    fn a_reset_does_not_carry_a_morning_report() {
        let reset = event(
            3,
            "morning",
            togi_journal::CommandReset {
                target: togi_journal::ResetTarget::All,
            },
        );
        assert_eq!(Report::of(&[night(), day(2, "morning"), reset]), None);
    }
}
