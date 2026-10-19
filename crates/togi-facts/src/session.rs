use jiff::Timestamp;
use togi_journal::{Build, EventKind, ResetTarget, Seq, SessionId, TrialIntent};
use togi_machine::{BiosContext, BootId, CoreId, CoreInfo, CoreSet, PerCore};

use crate::fact::Fact;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Effect {
    pub seq: Seq,
    pub kind: EventKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reset {
    pub seq: Seq,
    pub target: ResetTarget,
    pub effects: Vec<Effect>,
    pub applied: Option<Seq>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResetCutoffs {
    all: Option<Seq>,
    cores: PerCore<Option<Seq>>,
}

impl ResetCutoffs {
    pub const fn all(&self) -> Option<Seq> {
        self.all
    }

    pub fn core(&self, core: CoreId) -> Option<Seq> {
        self.cores.get(core).flatten()
    }

    pub(crate) fn record(&mut self, seq: Seq, target: ResetTarget) {
        match target {
            ResetTarget::All => self.all = Some(seq),
            ResetTarget::Core(core) => {
                self.cores.set(core, Some(seq));
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
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
    pub context: Option<BiosContext>,
    pub topology: Vec<CoreInfo>,
    pub cores: CoreSet,
    pub facts: Vec<Fact>,
    pub carried: Vec<Fact>,
    pub resets: Vec<Reset>,
    pub cutoffs: ResetCutoffs,
    pub open_trials: Vec<TrialActivity>,
    pub carry_commit: Option<Seq>,
    pub shutdown: Option<Seq>,
}

impl SessionFacts {
    pub const fn epoch(&self) -> u32 {
        self.build.evidence
    }
}
