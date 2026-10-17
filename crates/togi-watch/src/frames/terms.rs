//! The names the narration, the trial band and the panels all give a trial and a cycle's parts, so each is
//! written one way.

use std::fmt::Write as _;

use togi_journal::{DeadEndCondition, ShutdownReason};
use togi_machine::{Condition, Regime};
use togi_tuner::{CyclePart, CycleStep};

use crate::model::TrialView;
use crate::words::{core_ids, kind_words};

/// Why the session stopped, in a clause.
pub(crate) fn stop_words(reason: ShutdownReason) -> &'static str {
    match reason {
        ShutdownReason::Signal => "stopped by a signal",
        ShutdownReason::Cycles => "requested clean cycles completed",
        ShutdownReason::FirstResult => "stopped at the first confirmed result",
        ShutdownReason::Command => "read-only command finished; no applied offsets changed",
        ShutdownReason::DeadEnd => "tuning stopped at a dead end",
    }
}

/// Why tuning cannot continue, in a clause.
pub(crate) fn dead_end_words(condition: DeadEndCondition) -> &'static str {
    match condition {
        DeadEndCondition::FailureAtZero => "a core failed at offset 0",
        DeadEndCondition::Smu => "the CPU did not take an offset as written",
        DeadEndCondition::NoEvidence => "no conclusive trial evidence",
        DeadEndCondition::BootLoop => "repeated crashes before any offset was applied",
        DeadEndCondition::Containment => "stress programs not confirmed contained",
        DeadEndCondition::Preflight => "a check before tuning failed",
        DeadEndCondition::Defect => "an earlier build may have moved offsets deeper than proven",
        DeadEndCondition::ThermalTrip => "the CPU shut down from heat",
    }
}

/// A trial's regime, kind and cores: `R7 all-core on 00-15`, or `core 03 · R1 light vector at -20` alone.
pub(crate) fn trial_name(trial: &TrialView) -> String {
    match (trial.condition, trial.core, trial.offset) {
        (Condition::Alone, Some(core), Some(offset)) => format!(
            "core {:02} · {} {} at {}",
            core.get(),
            trial.regime,
            kind_words(trial.regime),
            offset.get()
        ),
        _ => format!(
            "{} {} on {}",
            trial.regime,
            kind_words(trial.regime),
            core_ids(trial.cores)
        ),
    }
}

/// Where a checking trial stood in its cycle: `cycle 1 step 2, part 5, trial 4 of 4`; empty outside a cycle.
pub(crate) fn cycle_place(trial: &TrialView) -> String {
    let Some(place) = trial.cycle else {
        return String::new();
    };
    let mut text = format!("cycle {}", place.cycle);
    if let Some(step) = place.step {
        let _ = write!(text, " step {step}");
    }
    if let Some(part) = place.part.filter(|_| place.parts > 1) {
        let _ = write!(text, ", part {part}");
    }
    if trial.count.of > 0 {
        let _ = write!(text, ", trial {} of {}", trial.count.index, trial.count.of);
    }
    text
}

/// A cycle part's name: its cores outside R7, else `full CCD 1` or `partial CCD 0+1`.
pub(crate) fn part_name(regime: Regime, part: &CyclePart) -> String {
    if regime != Regime::R7 {
        return format!("core {}", core_ids(part.cores));
    }
    let label = if part.full { "full" } else { "partial" };
    match part.ccd {
        Some(ccd) => format!("{label} CCD {}", ccd.0),
        None => format!("{label} CCD 0+1"),
    }
}

/// The running part of `step`, or else its first part not done, counted from 1.
pub(crate) fn first_open_part(step: &CycleStep) -> Option<usize> {
    step.parts
        .iter()
        .position(|part| part.running)
        .or_else(|| step.parts.iter().position(|part| !part.done))
        .map(|index| index + 1)
}

/// That a hunt cut its parts finer: `re-cut 2 · `, or `RE-CUT 2 · ` in `upper` case. `cut` is the split's
/// number among the hunt's splits; the first split re-cut nothing, so it says nothing.
pub(crate) fn recut_words(cut: u32, upper: bool) -> String {
    match cut.checked_sub(1).filter(|&n| n > 0) {
        Some(n) if upper => format!("RE-CUT {n} · "),
        Some(n) => format!("re-cut {n} · "),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use togi_journal::DeadEndCondition;
    use togi_machine::{CcdId, CoreId, CoreSet, Regime};
    use togi_tuner::{CyclePart, CycleStep};

    use super::{dead_end_words, first_open_part, part_name, recut_words};

    fn part(cores: &[u8], ccd: Option<u8>, full: bool, done: bool, running: bool) -> CyclePart {
        CyclePart {
            cores: cores
                .iter()
                .map(|&id| CoreId::new(i64::from(id)).expect("a core id"))
                .collect::<CoreSet>(),
            ccd: ccd.map(CcdId),
            full,
            partial: !full,
            short: 1,
            short_s: 60,
            long: 0,
            long_s: 0,
            passed: 0,
            failed: 0,
            done,
            running,
        }
    }

    #[test]
    fn parts_are_named_by_their_cores_or_their_ccd() {
        assert_eq!(
            part_name(Regime::R3, &part(&[0, 1, 2], None, true, false, false)),
            "core 00-02"
        );
        assert_eq!(
            part_name(Regime::R7, &part(&[0], Some(1), true, false, false)),
            "full CCD 1"
        );
        assert_eq!(
            part_name(Regime::R7, &part(&[0], None, false, false, false)),
            "partial CCD 0+1"
        );
    }

    #[test]
    fn the_open_part_is_the_running_one_or_the_first_not_done() {
        let step = |parts| CycleStep {
            regime: Regime::R7,
            workload: togi_machine::WorkloadId::new(Regime::R7, 0).expect("an R7 workload"),
            parts,
            chains_complete: false,
            done: false,
        };
        let a = part(&[0], Some(0), true, true, false);
        let b = part(&[1], Some(1), true, false, false);
        let c = part(&[1], Some(1), false, false, true);
        assert_eq!(
            first_open_part(&step(vec![a.clone(), b.clone(), c])),
            Some(3)
        );
        assert_eq!(first_open_part(&step(vec![a.clone(), b])), Some(2));
        assert_eq!(first_open_part(&step(vec![a])), None);
    }

    #[test]
    fn the_first_split_is_no_recut() {
        assert_eq!(recut_words(0, false), "");
        assert_eq!(recut_words(1, true), "");
        assert_eq!(recut_words(2, false), "re-cut 1 · ");
        assert_eq!(recut_words(3, true), "RE-CUT 2 · ");
    }

    #[test]
    fn every_dead_end_cause_has_its_own_true_explanation() {
        let table = [
            (DeadEndCondition::FailureAtZero, "a core failed at offset 0"),
            (
                DeadEndCondition::Smu,
                "the CPU did not take an offset as written",
            ),
            (DeadEndCondition::NoEvidence, "no conclusive trial evidence"),
            (
                DeadEndCondition::BootLoop,
                "repeated crashes before any offset was applied",
            ),
            (
                DeadEndCondition::Containment,
                "stress programs not confirmed contained",
            ),
            (DeadEndCondition::Preflight, "a check before tuning failed"),
            (
                DeadEndCondition::Defect,
                "an earlier build may have moved offsets deeper than proven",
            ),
            (DeadEndCondition::ThermalTrip, "the CPU shut down from heat"),
        ];
        let causes: Vec<DeadEndCondition> = table.iter().map(|&(cause, _)| cause).collect();
        assert_eq!(causes, DeadEndCondition::ALL, "every cause is in the table");
        for (cause, words) in table {
            assert_eq!(dead_end_words(cause), words, "{cause}");
        }
    }
}
