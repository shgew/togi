use std::path::{Path, PathBuf};

use serde::Serialize;
use togi_machine::object_only;
use togi_machine::{Backend, PerCore, Regime, TargetOffset};

object_only! {
    /// The effective configuration a session recorded in `config.loaded`.
    ///
    /// The journal owns this shape, so a change to the configuration crate cannot change the journal format. It
    /// records values, not policy: the configuration crate validates them when a recorded snapshot is read back.
    ///
    /// The five `retired` members were Ruleset 12's settings. Ruleset 13 never writes them, but a journal that
    /// recorded them still decodes, so historical readers see what those sessions ran with.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ConfigSnapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub start_offsets: Option<PerCore<TargetOffset>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub candidate_solo_limits: Option<PerCore<TargetOffset>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub durations: Option<ConfigDurations>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evidence: Option<ConfigEvidence>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub checking: Option<ConfigChecking>,
        pub dead_ends: ConfigDeadEnds,
        pub backends: ConfigBackends,
        pub backend_user: Option<String>,
    }
}

object_only! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ConfigDurations {
        pub search_trial_s: u32,
        pub short_trial_s: u32,
        pub checking_trial_s: u32,
        pub checking_idle_s: u32,
        pub checking_all_core_s: u32,
    }
}

object_only! {
    #[derive(Clone, Copy, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ConfigEvidence {
        pub miss: f64,
        pub rate: f64,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ConfigChecking {
        pub cycle: Vec<Regime>,
    }
}

object_only! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ConfigDeadEnds {
        pub inconclusive_in_a_row: u32,
        pub stray_crashes_in_a_row: u32,
    }
}

object_only! {
    #[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ConfigBackends {
        pub mprime: Option<PathBuf>,
        pub ycruncher: Option<PathBuf>,
    }
}

impl ConfigBackends {
    pub fn path(&self, backend: Backend) -> Option<&Path> {
        match backend {
            Backend::Mprime => self.mprime.as_deref(),
            Backend::Ycruncher => self.ycruncher.as_deref(),
        }
    }
}
