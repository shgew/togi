use serde_json::json;
use togi_tool_audit::Check::Termination;
use togi_tool_audit::Completion::{Finished, InProgress};

use crate::fixtures::*;

#[test]
fn a_finished_run_ends_in_a_conclusion() {
    run(vec![
        case("a shutdown", vec![stop(2)], Finished, &[]),
        case(
            "a dead end",
            vec![dead_end(2, "unsupported machine")],
            Finished,
            &[],
        ),
        case(
            "an archive followed by a projection warning",
            vec![phase(2, 0, None), archived(3), warning(4)],
            Finished,
            &[],
        ),
        case("a reset of every core", vec![reset_all(2)], Finished, &[]),
        case(
            "a reset of one core concludes nothing",
            vec![reset_core(2, 0)],
            Finished,
            &[(2, Termination)],
        ),
        case(
            "no conclusion",
            vec![phase(2, 0, None)],
            Finished,
            &[(2, Termination)],
        ),
        case(
            "a dead end without a reason",
            vec![dead_end(2, "")],
            Finished,
            &[(2, Termination)],
        ),
        case(
            "an open journal owes no conclusion",
            vec![phase(2, 0, None)],
            InProgress,
            &[],
        ),
        case(
            "a projection warning after the shutdown keeps the conclusion",
            vec![stop(2), warning(3), warning(4)],
            Finished,
            &[],
        ),
        case(
            "projection warnings alone conclude nothing",
            vec![warning(2), warning(3)],
            Finished,
            &[(3, Termination)],
        ),
        case(
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
        ),
        case(
            "an event after the shutdown reopens the run",
            vec![stop(2), phase(3, 0, None)],
            Finished,
            &[(3, Termination)],
        ),
    ]);
}

#[test]
fn a_shutdown_agrees_with_the_events_that_led_to_it() {
    run(vec![
        case(
            "a dead-end shutdown after its dead end",
            vec![dead_end(2, "d"), shutdown(3, json!({"reason": "dead_end"}))],
            Finished,
            &[],
        ),
        case(
            "a dead-end shutdown without a dead end",
            vec![shutdown(2, json!({"reason": "dead_end"}))],
            Finished,
            &[(2, Termination)],
        ),
        case(
            "a dead end is consumed by its shutdown",
            vec![
                dead_end(2, "d"),
                shutdown(3, json!({"reason": "dead_end"})),
                shutdown(4, json!({"reason": "dead_end"})),
            ],
            Finished,
            &[(4, Termination)],
        ),
        case(
            "a cycles shutdown after the requested passed full cycles",
            vec![
                cycle_end(2, true, true),
                cycle_end(3, true, true),
                shutdown(4, json!({"reason": "cycles", "cycles": 2})),
            ],
            Finished,
            &[],
        ),
        case(
            "a cycles shutdown before enough passed full cycles",
            vec![
                cycle_end(2, true, true),
                cycle_end(3, false, true),
                cycle_end(4, true, false),
                shutdown(5, json!({"reason": "cycles", "cycles": 2})),
            ],
            Finished,
            &[(5, Termination)],
        ),
        case(
            "a cycles shutdown that records no count",
            vec![
                cycle_end(2, true, true),
                shutdown(3, json!({"reason": "cycles"})),
            ],
            Finished,
            &[(3, Termination)],
        ),
        case(
            "a signal shutdown needs no cause",
            vec![shutdown(2, json!({"reason": "signal"}))],
            Finished,
            &[],
        ),
    ]);
}
