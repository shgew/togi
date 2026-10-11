use std::num::NonZeroU32;

use togi_journal::{Activity, TrialIntent};
use togi_machine::Condition;

use crate::state::Run;
use crate::{ClassKey, Profile, Requirement};

impl Run {
    pub(crate) fn hunt_intent(&self, intent: &TrialIntent, class: ClassKey) -> Option<Requirement> {
        let (Some(hunt), Some(group)) = (intent.hunt, intent.group) else {
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
        let matches = intent.condition == Condition::Parked
            && intent.activity == Some(Activity::Hunt)
            && !intent.rerun
            && intent.cycle.is_none()
            && intent.step.is_none()
            && intent.round.is_none()
            && class == latest.class
            && Profile::new(&self.topo, intent.profile).ok() == Some(latest.profile);
        matches.then(|| self.hunt_requirement(active, latest))
    }

    pub(crate) fn hunt_retry_requirement(
        &self,
        hunt: NonZeroU32,
        group: NonZeroU32,
    ) -> Option<Requirement> {
        let active = self
            .hunts
            .active
            .as_ref()
            .filter(|active| active.id == hunt)?;
        let latest = active
            .latest()
            .filter(|latest| latest.payload.group == group)?;
        Some(self.hunt_requirement(active, latest))
    }
}
