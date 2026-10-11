use std::fmt;

use crate::error::InvalidId;
use crate::plan::InstanceName;

pub(crate) const SCOPE_PREFIX: &str = "togi-trial-";

fn allowed(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')
}

/// A trial id: one path component and one unit-name component.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TrialId(String);

impl TrialId {
    pub(crate) fn parse(text: &str) -> Result<Self, InvalidId> {
        if text.is_empty() {
            return Err(InvalidId::Empty);
        }
        match text.chars().find(|c| !allowed(*c)) {
            Some(c) => Err(InvalidId::Character(c)),
            None if text == "." || text == ".." => Err(InvalidId::Character('.')),
            None => Ok(Self(text.to_owned())),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TrialId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A concrete trial scope's name without the `.scope` suffix.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ScopeName(String);

impl ScopeName {
    pub(crate) fn for_instance(id: &TrialId, instance: InstanceName, per_core: bool) -> Self {
        match (per_core, instance) {
            (true, InstanceName::Core(core)) => {
                Self(format!("{SCOPE_PREFIX}{id}-c{:02}", core.get()))
            }
            _ => Self(format!("{SCOPE_PREFIX}{id}")),
        }
    }

    /// Accepts exactly `togi-trial-<[A-Za-z0-9_.:-]+>.scope`.
    pub(crate) fn from_unit(unit: &str) -> Option<Self> {
        let name = unit.strip_suffix(".scope")?;
        let id = name.strip_prefix(SCOPE_PREFIX)?;
        (!id.is_empty() && id.chars().all(allowed)).then(|| Self(name.to_owned()))
    }

    pub(crate) fn unit(&self) -> String {
        format!("{}.scope", self.0)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ScopeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
