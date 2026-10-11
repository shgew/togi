use crate::record::{Class, Profile, TrialFact};

/// What a fact answers for: the full applied profile and the class. Provenance, condition, phase, outcome
/// and measurements are not part of it; the profile is compared as recorded and the loaded cores are a set,
/// so neither needs sorting first.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProfileClassKey {
    pub profile: Profile,
    pub class: Class,
}

impl From<&TrialFact> for ProfileClassKey {
    fn from(fact: &TrialFact) -> Self {
        Self {
            profile: fact.profile,
            class: fact.class,
        }
    }
}
