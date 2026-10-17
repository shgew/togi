//! Hunts: the partition plan that narrows an unattributed failure to a culprit or a combination,
//! the parked profiles its groups run at, and the singleton/member probes and backoff that follow,
//! with the folded state and the decisions that drive them.

mod evidence;
mod fold;
mod next;
pub(crate) mod plan;
mod retry;
pub(crate) mod state;
mod view;

pub(crate) use state::Hunts;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod hunt_tests;
#[cfg(test)]
mod locate_tests;
#[cfg(test)]
mod property_tests;
#[cfg(test)]
mod provenance_tests;
#[cfg(test)]
mod smoke_tests;
#[cfg(test)]
mod support;
