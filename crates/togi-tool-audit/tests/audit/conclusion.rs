use serde_json::json;
use togi_tool_audit::Check::Termination;
use togi_tool_audit::Completion::{Finished, InProgress};

use crate::fixtures::*;

#[test_case::test_case(case("a shutdown", vec![stop(2)], Finished, &[]); "a shutdown")]
#[test_case::test_case(case(
            "a dead end",
            vec![dead_end(2, "unsupported machine")],
            Finished,
            &[],
        ); "a dead end")]
#[test_case::test_case(case(
            "an archive followed by a projection warning",
            vec![phase(2, 0, None), archived(3), warning(4)],
            Finished,
            &[],
        ); "an archive followed by a projection warning")]
#[test_case::test_case(case("a reset of every core", vec![reset_all(2)], Finished, &[]); "a reset of every core")]
#[test_case::test_case(case(
            "a reset of one core concludes nothing",
            vec![reset_core(2, 0)],
            Finished,
            &[(2, Termination)],
        ); "a reset of one core concludes nothing")]
#[test_case::test_case(case(
            "no conclusion",
            vec![phase(2, 0, None)],
            Finished,
            &[(2, Termination)],
        ); "no conclusion")]
#[test_case::test_case(case(
            "a dead end without a reason",
            vec![dead_end(2, "")],
            Finished,
            &[(2, Termination)],
        ); "a dead end without a reason")]
#[test_case::test_case(case(
            "an open journal owes no conclusion",
            vec![phase(2, 0, None)],
            InProgress,
            &[],
        ); "an open journal owes no conclusion")]
#[test_case::test_case(case(
            "a projection warning after the shutdown keeps the conclusion",
            vec![stop(2), warning(3), warning(4)],
            Finished,
            &[],
        ); "a projection warning after the shutdown keeps the conclusion")]
#[test_case::test_case(case(
            "projection warnings alone conclude nothing",
            vec![warning(2), warning(3)],
            Finished,
            &[(3, Termination)],
        ); "projection warnings alone conclude nothing")]
#[test_case::test_case(case(
            "a boot's leave reason after the shutdown keeps the conclusion",
            vec![
                stop(2),
                event(
                    3,
                    "boot.leave_reason",
                    json!({"reason_id": "1", "restart_limit_count": 1, "reason": "r"}),
                ),
            ],
            Finished,
            &[],
        ); "a boot's leave reason after the shutdown keeps the conclusion")]
#[test_case::test_case(case(
            "a day lifecycle after the shutdown keeps the conclusion",
            vec![
                stop(2),
                event(
                    3,
                    "day.boot_start",
                    json!({
                        "build": {"version": "0.1.0", "rev": "abc1234", "ruleset": 10, "schema": 1, "fixes": 2, "evidence": 1},
                        "bios": {"bios_version": "F31", "board": "X570", "cpu_model": "5950X", "microcode": "0xa201016", "boost_limit_mhz": 5000},
                        "offsets": {"0": 0},
                    }),
                ),
                event(4, "day.checkpoint", json!({"applied_secs": 3600})),
                event(5, "day.offsets_changed", json!({"offsets": {"0": -10}})),
                event(6, "day.clean_stop", json!({})),
                event(
                    7,
                    "day.unclean_end",
                    json!({
                        "boot": "7a1b2c3d-0000-0000-0000-000000000000",
                        "last_checkpoint": 4,
                        "offsets": {"0": -10},
                        "reset_reason": {"kind": "watchdog", "raw": ["watchdog"], "supported": true},
                    }),
                ),
            ],
            Finished,
            &[],
        ); "a day lifecycle after the shutdown keeps the conclusion")]
#[test_case::test_case(case(
            "an event after the shutdown reopens the run",
            vec![stop(2), phase(3, 0, None)],
            Finished,
            &[(3, Termination)],
        ); "an event after the shutdown reopens the run")]
fn a_finished_run_ends_in_a_conclusion(case: Case) {
    run(case);
}

#[test_case::test_case(case(
            "a dead-end shutdown after its dead end",
            vec![dead_end(2, "d"), shutdown(3, json!({"reason": "dead_end"}))],
            Finished,
            &[],
        ); "a dead-end shutdown after its dead end")]
#[test_case::test_case(case(
            "a dead-end shutdown without a dead end",
            vec![shutdown(2, json!({"reason": "dead_end"}))],
            Finished,
            &[(2, Termination)],
        ); "a dead-end shutdown without a dead end")]
#[test_case::test_case(case(
            "a dead end is consumed by its shutdown",
            vec![
                dead_end(2, "d"),
                shutdown(3, json!({"reason": "dead_end"})),
                shutdown(4, json!({"reason": "dead_end"})),
            ],
            Finished,
            &[(4, Termination)],
        ); "a dead end is consumed by its shutdown")]
#[test_case::test_case(case(
            "a cycles shutdown while a dead end awaits its own shutdown",
            vec![
                cycle_end(2, true, true),
                dead_end(3, "d"),
                shutdown(4, json!({"reason": "cycles", "cycles": 1})),
            ],
            Finished,
            &[(4, Termination)],
        ); "a cycles shutdown while a dead end awaits its own shutdown")]
#[test_case::test_case(case(
            "a first-result shutdown while a dead end awaits its own shutdown",
            vec![
                dead_end(2, "d"),
                shutdown(3, json!({"reason": "first_result"})),
            ],
            Finished,
            &[(3, Termination)],
        ); "a first-result shutdown while a dead end awaits its own shutdown")]
#[test_case::test_case(case(
            "a signal interrupts a dead end, and the next run completes it",
            vec![
                dead_end(2, "d"),
                shutdown(3, json!({"reason": "signal"})),
                shutdown(4, json!({"reason": "dead_end"})),
            ],
            Finished,
            &[],
        ); "a signal interrupts a dead end, and the next run completes it")]
#[test_case::test_case(case(
            "a cycles shutdown after the requested passed full cycles",
            vec![
                cycle_end(2, true, true),
                cycle_end(3, true, true),
                shutdown(4, json!({"reason": "cycles", "cycles": 2})),
            ],
            Finished,
            &[],
        ); "a cycles shutdown after the requested passed full cycles")]
#[test_case::test_case(case(
            "a cycles shutdown before enough passed full cycles",
            vec![
                cycle_end(2, true, true),
                cycle_end(3, false, true),
                cycle_end(4, true, false),
                shutdown(5, json!({"reason": "cycles", "cycles": 2})),
            ],
            Finished,
            &[(5, Termination)],
        ); "a cycles shutdown before enough passed full cycles")]
#[test_case::test_case(case(
            "a cycles shutdown that records no count",
            vec![
                cycle_end(2, true, true),
                shutdown(3, json!({"reason": "cycles"})),
            ],
            Finished,
            &[(3, Termination)],
        ); "a cycles shutdown that records no count")]
#[test_case::test_case(case(
            "a signal shutdown needs no cause",
            vec![shutdown(2, json!({"reason": "signal"}))],
            Finished,
            &[],
        ); "a signal shutdown needs no cause")]
fn a_shutdown_agrees_with_the_events_that_led_to_it(case: Case) {
    run(case);
}
