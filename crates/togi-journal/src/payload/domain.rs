use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! wire_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $wire)] $variant,)+ }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire,)+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

wire_enum!(
    /// What the session is doing when it asks for a trial or takes a decision.
    Activity { Search => "search", Checking => "checking", Hunt => "hunt", Deepening => "deepening" }
);
wire_enum!(
    /// Where a core stands in the search.
    CorePhase { Search => "search", HasRoom => "has_room", AtLimit => "at_limit" }
);
wire_enum!(Decision {
    StepDeeper => "step_deeper",
    CheckSoloLimit => "check_solo_limit",
    Deepen => "deepen",
    Yield => "yield",
    Backoff => "backoff",
    Hold => "hold",
});
wire_enum!(Outcome { Pass => "pass", Failure => "failure", Inconclusive => "inconclusive" });
wire_enum!(Attribution { Attributed => "attributed", Unattributed => "unattributed" });
wire_enum!(CycleEvent { Start => "start", End => "end" });
wire_enum!(RoundEvent { Start => "start", End => "end" });
wire_enum!(HuntStage {
    Part => "part",
    Complement => "complement",
    Full => "full",
    Locate => "locate",
    Probe => "probe",
});
wire_enum!(HuntOutcome {
    Cancelled => "cancelled",
    Direct => "direct",
    Culprit => "culprit",
    Combination => "combination",
    Fallback => "fallback",
    Loaded => "loaded",
});
wire_enum!(Inference { Pass => "pass", Failure => "failure" });
wire_enum!(WarningKind { Monotonicity => "monotonicity" });
wire_enum!(DeadEndCondition {
    FailureAtZero => "failure_at_zero",
    Smu => "smu",
    NoEvidence => "no_evidence",
    BootLoop => "boot_loop",
    Containment => "containment",
    Preflight => "preflight",
    Defect => "defect",
    ThermalTrip => "thermal_trip",
});
wire_enum!(DeadEndAction {
    Exit => "exit",
    ClearSavedEntry => "clear_saved_entry",
    ClearSavedEntryAndReboot => "clear_saved_entry_and_reboot",
});
wire_enum!(DefectDirection { TooCautious => "too_cautious", TooAggressive => "too_aggressive" });
wire_enum!(DefectAnswer { Yes => "yes", No => "no" });
wire_enum!(ShutdownReason {
    Signal => "signal",
    DeadEnd => "dead_end",
    Cycles => "cycles",
    Command => "command",
    FirstResult => "first_result",
});
wire_enum!(Notice { NonzeroBaseline => "nonzero_baseline" });
wire_enum!(SmuOp { Set => "set", SetAll => "set_all", Read => "read" });
wire_enum!(
    /// What a backend retry waits on: a workload backend or the kernel log.
    RetryTarget { Mprime => "mprime", Ycruncher => "ycruncher", KernelLog => "kernel_log" }
);
