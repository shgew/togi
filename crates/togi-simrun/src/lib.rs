//! Drives a tuning session on the simulator: a crash reboots the simulated machine and the next boot resumes the
//! journal, until a run ends without one or the boot cap is reached.

mod error;
mod history;
mod input;
mod observe;
mod simulate;

pub use error::{HistoryError, SimError};
pub use history::{history, history_in, recorded_config, recorded_config_in};
pub use input::{Finished, Observe, Observer, Persistence, Replay, SimInput, SimOutcome};
pub use simulate::{events, simulate};
