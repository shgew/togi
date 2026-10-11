use togi_machine::{ResetKind, ResetReason};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CrashKind {
    InTrial,
    Idle,
    Stray,
    Inconclusive,
    Thermal,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent facts of one boot, named as the spec names them"
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrashFacts {
    pub in_trial: bool,
    pub applied: bool,
    /// A backend signal, a trial-window MCE or an uncorrected MCE in the next boot.
    pub evidence: bool,
    pub reason: ResetReason,
    pub confirmed: bool,
}

pub fn classify_crash(facts: &CrashFacts) -> CrashKind {
    let ordinary = if facts.in_trial {
        CrashKind::InTrial
    } else if facts.applied {
        CrashKind::Idle
    } else {
        CrashKind::Stray
    };
    if facts.evidence {
        return ordinary;
    }
    match facts.reason.kind {
        Some(ResetKind::ThermalTrip) => CrashKind::Thermal,
        Some(ResetKind::PowerButton) if facts.in_trial => CrashKind::InTrial,
        Some(ResetKind::PowerButton) => CrashKind::Inconclusive,
        None if facts.reason.supported && facts.confirmed => CrashKind::Inconclusive,
        None
        | Some(
            ResetKind::Watchdog
            | ResetKind::SyncFlood
            | ResetKind::CpuShutdown
            | ResetKind::PowerLoss
            | ResetKind::Unknown,
        ) => ordinary,
    }
}

/// Reads back the kind `classify_crash` gave the crash a `crash.detected` event records;
/// `in_trial` says whether a trial was open in the crashed boot.
pub fn recorded_kind(
    stray: bool,
    inconclusive: bool,
    reason: &ResetReason,
    in_trial: bool,
) -> CrashKind {
    if stray {
        CrashKind::Stray
    } else if inconclusive && reason.kind == Some(ResetKind::ThermalTrip) {
        CrashKind::Thermal
    } else if inconclusive {
        CrashKind::Inconclusive
    } else if in_trial {
        CrashKind::InTrial
    } else {
        CrashKind::Idle
    }
}
