use jiff::Timestamp;
use togi_journal::{Build, Seq, SessionId, TrialIntent};
use togi_machine::{BootId, CoreInfo, CoreSet};

use crate::fact::Fact;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrialActivity {
    pub seq: Seq,
    pub time: Timestamp,
    pub boot: BootId,
    pub intent: TrialIntent,
    pub started: bool,
    pub last_evidence: Timestamp,
    pub crashed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionFacts {
    pub id: SessionId,
    pub build: Build,
    pub topology: Vec<CoreInfo>,
    pub cores: CoreSet,
    pub facts: Vec<Fact>,
    pub carried: Vec<Fact>,
    pub open_trials: Vec<TrialActivity>,
    pub shutdown: Option<Seq>,
}

impl SessionFacts {
    pub const fn epoch(&self) -> u32 {
        self.build.evidence
    }
}
