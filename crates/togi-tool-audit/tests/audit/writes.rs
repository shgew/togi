use serde_json::json;
use togi_journal::Event;
use togi_tool_audit::Check::WriteProtocol;
use togi_tool_audit::Completion::{Finished, InProgress};

use crate::fixtures::*;

#[test]
fn a_write_follows_its_intent_and_is_read_back() {
    run(vec![
        case(
            "a set-all read back on every core",
            vec![
                intent(2, set_all(0)),
                write(3, set_all(0), 2),
                readback(4, 0, 0, Some(3)),
                readback(5, 8, 0, Some(3)),
                stop(6),
            ],
            Finished,
            &[],
        ),
        case(
            "a write without an intent",
            vec![
                event(2, "smu.write", set(0, 0)),
                readback(3, 0, 0, Some(2)),
                stop(4),
            ],
            Finished,
            &[(2, WriteProtocol)],
        ),
        case(
            "a write citing the intent of another core",
            vec![
                intent(2, set(8, 0)),
                write(3, set(0, 0), 2),
                readback(4, 0, 0, Some(3)),
                stop(5),
            ],
            Finished,
            &[(3, WriteProtocol)],
        ),
        case(
            "a write citing the intent of another offset",
            vec![
                intent(2, set(0, -5)),
                write(3, set(0, -6), 2),
                readback(4, 0, -6, Some(3)),
                stop(5),
            ],
            Finished,
            &[(3, WriteProtocol)],
        ),
        case(
            "a set-all missing one core's readback",
            vec![
                intent(2, set_all(0)),
                write(3, set_all(0), 2),
                readback(4, 0, 0, Some(3)),
                stop(5),
            ],
            Finished,
            &[(3, WriteProtocol)],
        ),
        case(
            "a write still in flight in a live journal",
            vec![intent(2, set(0, 0)), write(3, set(0, 0), 2)],
            InProgress,
            &[],
        ),
        case(
            "an unread write is judged when the next write begins",
            vec![
                intent(2, set(0, -5)),
                write(3, set(0, -5), 2),
                intent(4, set(0, -4)),
                write(5, set(0, -4), 4),
                readback(6, 0, -4, Some(5)),
            ],
            InProgress,
            &[(3, WriteProtocol)],
        ),
    ]);
}

#[test]
fn an_intent_authorizes_one_write_in_its_own_boot() {
    run(vec![
        case(
            "an intent of an earlier boot",
            vec![
                intent(2, set(0, 0)),
                boot(write(3, set(0, 0), 2), "c"),
                boot(readback(4, 0, 0, Some(3)), "c"),
                boot(stop(5), "c"),
            ],
            Finished,
            &[(3, WriteProtocol)],
        ),
        case(
            "an intent used twice",
            vec![
                intent(2, set(0, 0)),
                write(3, set(0, 0), 2),
                readback(4, 0, 0, Some(3)),
                write(5, set(0, 0), 2),
                readback(6, 0, 0, Some(5)),
                stop(7),
            ],
            Finished,
            &[(5, WriteProtocol)],
        ),
    ]);
}

#[test]
fn a_reboot_may_interrupt_a_write_before_its_readback() {
    run(vec![
        case(
            "the next boot owes no readback",
            vec![
                intent(2, set(0, -5)),
                write(3, set(0, -5), 2),
                boot(phase(4, 0, None), "c"),
                boot(stop(5), "c"),
            ],
            Finished,
            &[],
        ),
        case(
            "same-boot reconciliation reads every core without citing the write",
            vec![
                intent(2, set(0, -5)),
                write(3, set(0, -5), 2),
                readback(4, 0, -5, None),
                readback(5, 8, 0, None),
                restored(6, [0, 0]),
                stop(7),
            ],
            Finished,
            &[],
        ),
    ]);
}

#[test]
fn a_readback_matches_the_write_it_cites() {
    run(vec![
        case(
            "a readback of a core the write did not touch",
            vec![
                intent(2, set(0, -5)),
                write(3, set(0, -5), 2),
                readback(4, 0, -5, Some(3)),
                readback(5, 8, -5, Some(3)),
            ],
            InProgress,
            &[(5, WriteProtocol)],
        ),
        case(
            "a readback expecting another offset than the write's",
            vec![
                intent(2, set(0, -5)),
                write(3, set(0, -5), 2),
                cause(
                    event(
                        4,
                        "smu.readback",
                        json!({"core": 0, "offset": -4, "expected": -4}),
                    ),
                    &[3],
                ),
            ],
            InProgress,
            &[(4, WriteProtocol)],
        ),
        case(
            "a mismatching observed offset is the session's dead end, not a protocol breach",
            vec![
                intent(2, set(0, -5)),
                write(3, set(0, -5), 2),
                cause(
                    event(
                        4,
                        "smu.readback",
                        json!({"core": 0, "offset": -3, "expected": -5}),
                    ),
                    &[3],
                ),
            ],
            InProgress,
            &[],
        ),
        case(
            "firmware offsets in a readback and baseline are not bounded",
            vec![
                event(2, "smu.readback", json!({"core": 0, "offset": 10})),
                cause(
                    event(3, "session.baseline", json!({"offsets": offsets([10, 0])})),
                    &[2],
                ),
            ],
            InProgress,
            &[],
        ),
    ]);
}

#[test]
fn a_mismatching_readback_ends_the_session_in_an_smu_dead_end() {
    let mismatch = || {
        vec![
            intent(2, set(0, -5)),
            write(3, set(0, -5), 2),
            cause(
                event(
                    4,
                    "smu.readback",
                    json!({"core": 0, "offset": -3, "expected": -5}),
                ),
                &[3],
            ),
        ]
    };
    let then = |rest: Vec<Event>| {
        let mut events = mismatch();
        events.extend(rest);
        events
    };
    let smu_dead_end = |seq| {
        cause(
            event(
                seq,
                "deadend",
                json!({"condition": "smu", "detail": "readback -3, expected -5"}),
            ),
            &[4],
        )
    };
    run(vec![
        case(
            "the smu dead end and its shutdown follow the mismatch",
            then(vec![
                smu_dead_end(5),
                shutdown(6, json!({"reason": "dead_end"})),
            ]),
            Finished,
            &[],
        ),
        case(
            "an ordinary shutdown follows the mismatch",
            then(vec![stop(5)]),
            Finished,
            &[(4, WriteProtocol)],
        ),
        case(
            "another dead end does not answer the mismatch",
            then(vec![
                dead_end(5, "d"),
                shutdown(6, json!({"reason": "dead_end"})),
            ]),
            Finished,
            &[(4, WriteProtocol)],
        ),
        case(
            "tuning continues with another write",
            then(vec![intent(5, set(0, -6))]),
            InProgress,
            &[(5, WriteProtocol)],
        ),
        case(
            "a crash before the dead end leaves it to the next boot",
            then(vec![boot(smu_dead_end(5), "b2")]),
            InProgress,
            &[],
        ),
    ]);
}
