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
    /// What the session is doing when it asks for a trial or takes a decision. `search`, `checking`, `hunt` and
    /// `deepening` are historical per-core activities; Ruleset 13 trials are `mapping`, `qualification`,
    /// `reproduction` and `control`.
    Activity {
        Search => "search",
        Checking => "checking",
        Hunt => "hunt",
        Deepening => "deepening",
        Mapping => "mapping",
        Qualification => "qualification",
        Reproduction => "reproduction",
        Control => "control",
    }
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
wire_enum!(
    /// A night's work queue, in the scheduler's visiting order (`docs/spec/tuner.md`, Night order); the filler
    /// runs only while no queue has due work.
    Queue {
        Reproduction => "reproduction",
        Qualification => "qualification",
        Mapping => "mapping",
        Control => "control",
        Requalification => "requalification",
        Filler => "filler",
    }
);
wire_enum!(
    /// Where a night task stands.
    TaskStatus {
        Pending => "pending",
        Running => "running",
        Complete => "complete",
        Cancelled => "cancelled",
    }
);
wire_enum!(
    /// When in a trial a failure happened: warm-up failures are retained but supply no positive-time rate.
    FailureTime { WarmUp => "warm_up", Eligible => "eligible" }
);
wire_enum!(
    /// The purpose of a trial's placement: isolated mapping, or the exact whole profile.
    Placement { Isolated => "isolated", Exact => "exact" }
);
wire_enum!(
    /// Why an exact profile is qualified.
    QualificationPurpose {
        NextStep => "next_step",
        Requalification => "requalification",
        Firmware => "firmware",
    }
);
wire_enum!(
    /// Why a class is uncovered for a CCD's cap: it has no observations, only non-mapping exposure, or
    /// mapping exposure that never met the adequate-exposure threshold.
    UncoveredReason {
        NoObservations => "no_observations",
        NonMappingOnly => "non_mapping_only",
        MappingInadequate => "mapping_inadequate",
    }
);
wire_enum!(
    /// Where a daily event's bounded reproduction task stands.
    ReproductionState {
        Pending => "pending",
        Reproduced => "reproduced",
        Unreproduced => "unreproduced",
        Untestable => "untestable",
    }
);
wire_enum!(
    /// Whether a CPU-suspect classification is definite or provisional (an unexplained end at a nonzero profile).
    Certainty { Definite => "definite", Provisional => "provisional" }
);
wire_enum!(
    /// Whether a whole-profile veto forbids its profile.
    VetoState { Active => "active", Cleared => "cleared" }
);
wire_enum!(
    /// An explicit freeze choice.
    FreezeChoice { Freeze => "freeze", Unfreeze => "unfreeze" }
);
wire_enum!(
    /// What an observed day interval counts as.
    IntervalState {
        Applied => "applied",
        Paused => "paused",
        Suspended => "suspended",
        Unobserved => "unobserved",
        Ambiguous => "ambiguous",
    }
);
wire_enum!(
    /// What kind of raw daily observation an import is.
    ImportClass {
        Boot => "boot",
        Checkpoint => "checkpoint",
        ProfileChange => "profile_change",
        Fault => "fault",
        End => "end",
        Answer => "answer",
        Manual => "manual",
    }
);
wire_enum!(
    /// A fact an imported record does not carry; the import never invents it.
    MissingFact {
        Build => "build",
        Firmware => "firmware",
        Profile => "profile",
        Readback => "readback",
        Elapsed => "elapsed",
        Workload => "workload",
    }
);
