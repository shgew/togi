//! R7 request order and attribution, self-sufficiency evidence, voltage-targeted backoff and
//! escalation to a located hunt, as pure values the tuner state folds into.

pub(crate) mod escalation;
pub(crate) mod failed;
pub(crate) mod measurements;
pub(crate) mod order;
pub(crate) mod probe;
pub(crate) mod query;
pub(crate) mod reason;
pub(crate) mod respond;
pub(crate) mod state;
pub(crate) mod status;
pub(crate) mod target;

#[cfg(test)]
mod tests;
