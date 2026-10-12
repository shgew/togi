use std::collections::BTreeMap;

use togi_machine::PerCore;
use togi_tool_bench::{Record, Split, Status};

/// A concluded dev run of `scenario` with every measurement zero but its ten simulated hours.
pub(crate) fn record(scenario: &str, seed: u64) -> Record {
    Record {
        scenario: scenario.to_owned(),
        machine: None,
        seed,
        split: Split::Dev,
        commit: "abc1234".to_owned(),
        dirty: false,
        ruleset: 1,
        evidence: 1,
        status: Status::Concluded,
        model_check: None,
        exit_code: 0,
        wall_s: Some(0.0),
        sim_hours: 10.0,
        first_passed_cycle_h: None,
        passed_cycles: 0,
        crashes: 0,
        trials: 0,
        real_answers: 0,
        real_answer_share: 0.0,
        scenario_real_answer_share: 0.0,
        trial_hours: 0.0,
        hunts: 0,
        combinations: 0,
        final_profile: PerCore::default(),
        depth: 0,
        hazard_per_h: BTreeMap::new(),
        hazard_max_per_h: 0.0,
        worst_r7_hazard_per_h: None,
    }
}
