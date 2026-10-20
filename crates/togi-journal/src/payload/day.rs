use serde::Serialize;
use togi_machine::object_only;
use togi_machine::{BiosContext, BootId, ObservedOffset, PerCore, ResetReason};

use crate::{Build, Seq};

object_only! {
    /// A `togi day` observer began watching this boot. The envelope's boot is the observed one.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayBootStart {
        pub build: Build,
        pub bios: BiosContext,
        /// The offsets read back as the observer started.
        pub offsets: PerCore<ObservedOffset>,
    }
}

object_only! {
    /// Periodic proof the observer is still running.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayCheckpoint {
        /// Whole seconds (floor) the observer has been active this boot, cumulative across its `day.boot_start`
        /// records; downtime between restarts is not credited.
        pub applied_secs: u64,
    }
}

object_only! {
    /// The offsets read back differ from the last ones the observer recorded.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayOffsetsChanged {
        pub offsets: PerCore<ObservedOffset>,
    }
}

object_only! {
    /// The observer stopped on request; its boot did not end.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayCleanStop {}
}

object_only! {
    /// The previous boot's observer never recorded a clean stop. The envelope's boot is the current one.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayUncleanEnd {
        /// The predecessor boot whose observer ended without `day.clean_stop`.
        pub boot: BootId,
        /// That boot's last `day.checkpoint`, if it wrote one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub last_checkpoint: Option<Seq>,
        /// The offsets that boot last recorded.
        pub offsets: PerCore<ObservedOffset>,
        pub reset_reason: ResetReason,
    }
}
