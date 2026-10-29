//! The authoritative candidate and limits shared by day and night policy.
//!
//! Ruleset 13 uses committed statistical caps. Callers do not select their source.
use super::{State, night_query::NotReady};
use crate::{
    UnitProfile,
    day::{DailyEvent, DayHistory, Proposal, next_proposal, shallower},
};

pub(super) struct Permission {
    pub(super) limits: UnitProfile,
}

impl Permission {
    pub(super) fn candidate(
        &self,
        history: &DayHistory,
        events: &[DailyEvent],
    ) -> Result<Option<Proposal>, NotReady> {
        Ok(next_proposal(history, events, &self.limits)?)
    }

    pub(super) fn clip(&self, wanted: &UnitProfile) -> Result<UnitProfile, NotReady> {
        Ok(shallower(wanted, &self.limits)?)
    }
}

impl State {
    pub(super) fn permission(&self) -> Result<Permission, NotReady> {
        Ok(Permission {
            limits: self.committed_caps()?,
        })
    }
}
