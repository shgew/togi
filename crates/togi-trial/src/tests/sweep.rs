//! The leftover sweep and the preflight probe on the fake OS under paused time.

use std::time::Duration;

use togi_machine::{CancellationToken, Stopped};
use tokio::task::spawn_local;
use tokio::time::{Instant, sleep};

use super::harness::{local, rig, rig_edit};
use crate::error::{CleanupFailure, Error};
use crate::id::ScopeName;
use crate::os::fake::{Reply, ScanFault};
use crate::os::{KillSignal, Os, Systemd};
use crate::procfs::ScopeProcess;
use crate::supervisor::Env;
use crate::sweep::{parse_listing, probe, sweep as run_sweep};
use crate::testing::TestBackend;
use crate::timing::Timing;

/// A sweep nobody cancels.
async fn sweep<O: Os>(env: &Env<O>) -> Result<(String, Stopped), Error> {
    run_sweep(env, &CancellationToken::new()).await
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn scope(name: &str) -> ScopeName {
    ScopeName::from_unit(&format!("{name}.scope")).unwrap()
}

fn process(scope_name: &str, pid: u32) -> ScopeProcess {
    ScopeProcess {
        scope: scope(scope_name),
        pid,
        starttime: 77,
    }
}

fn containment(error: &Error) -> &crate::Containment {
    match error {
        Error::Containment(containment) => containment,
        other => panic!("not containment: {other:?}"),
    }
}

#[test]
fn only_exact_trial_units_survive_the_listing_filter() {
    let output = "\
togi-trial-b.scope loaded active running x
togi-trial-a.scope loaded active running x
togi-preflight-9.scope loaded active running x
togi-trial-*.scope loaded active running x
togi-trial-.scope loaded active running x
xtogi-trial-a.scope loaded active running x
togi-trial-a.scope.bak loaded active running x
togi-trial-é.scope loaded active running x
user.slice loaded active running x
";
    let names: Vec<String> = parse_listing(output)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(names, ["togi-trial-b", "togi-trial-a"]);
}

fn calls(rig: &super::harness::Rig) -> Vec<(String, &'static str)> {
    rig.fake
        .systemd_calls()
        .iter()
        .filter_map(|(call, _)| match call {
            Systemd::Kill { scope, signal } => Some((scope.to_string(), signal.name())),
            Systemd::Stop { scope } => Some((scope.to_string(), "stop")),
            _ => None,
        })
        .collect()
}

#[test]
fn the_sweep_signals_each_exact_scope_in_order_and_removes_them_all() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-b.scope");
            rig.fake.add_unit("togi-trial-a.scope");
            rig.fake.add_unit("togi-preflight-9.scope");
            rig.fake.add_unit("togi-trial-*.scope");
            rig.fake.add_unit("unrelated.scope");
            let orphan = rig.fake.add_recovered(process("togi-trial-orphan", 4242));
            let duplicate = rig.fake.add_recovered(process("togi-trial-a", 4243));
            let (detail, _stopped) = sweep(&rig.env).await.unwrap();
            assert_eq!(
                detail,
                "removed 3 leftover trial scopes; no trial unit or process remains"
            );
            let scopes = ["togi-trial-a", "togi-trial-b", "togi-trial-orphan"];
            let mut expected = Vec::new();
            for signal in ["SIGCONT", "SIGTERM"] {
                expected.extend(scopes.map(|s| (s.to_owned(), signal)));
            }
            let ordered: Vec<_> = calls(&rig);
            assert_eq!(ordered[..6], expected[..]);
            let mut kills: Vec<_> = ordered[6..]
                .iter()
                .filter(|(_, what)| *what == "SIGKILL")
                .cloned()
                .collect();
            kills.sort();
            assert_eq!(kills, scopes.map(|s| (s.to_owned(), "SIGKILL")));
            let stops = ordered.iter().filter(|(_, what)| *what == "stop").count();
            assert_eq!(stops, 3);
            assert!(!orphan.alive.get() && !duplicate.alive.get());
            assert_eq!(
                rig.fake.units(),
                [
                    "togi-preflight-9.scope",
                    "togi-trial-*.scope",
                    "unrelated.scope"
                ]
            );
        })
        .await;
    });
}

#[test]
fn a_clean_machine_sweeps_to_success_without_signalling_anything() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let (detail, _stopped) = sweep(&rig.env).await.unwrap();
            assert_eq!(
                detail,
                "removed 0 leftover trial scopes; no trial unit or process remains"
            );
            assert!(calls(&rig).is_empty());
        })
        .await;
    });
}

#[test]
fn a_lingering_unit_or_a_process_moved_out_of_its_scope_is_containment_after_the_verify_slot() {
    crate::testing::runtime(true).block_on(async move {
        for moved_out in [false, true] {
            local(async {
                let rig = rig();
                let begun = Instant::now();
                if moved_out {
                    let recovered = rig.fake.add_recovered(process("togi-trial-x", 99));
                    rig.fake.add_unit("togi-trial-x.scope");
                    rig.fake.on_systemd(move |call| {
                        if matches!(call, Systemd::Kill { .. }) {
                            recovered.visible.set(false);
                        }
                        None
                    });
                } else {
                    rig.fake.add_unit("togi-trial-x.scope");
                    rig.fake.on_systemd(|call| match call {
                        Systemd::Stop { .. } | Systemd::Kill { .. } => Some(Reply::Output {
                            success: true,
                            text: String::new(),
                        }),
                        _ => None,
                    });
                }
                let error = sweep(&rig.env).await.err().unwrap();
                let elapsed = begun.elapsed();
                assert!(elapsed >= secs(10) && elapsed <= secs(15), "{elapsed:?}");
                let failures = containment(&error).failures();
                assert!(matches!(failures.last(), Some(CleanupFailure::Leftover)));
            })
            .await;
        }
    });
}

#[test]
fn a_hung_initial_listing_is_containment_at_the_shared_deadline() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_systemd(|call| match call {
                Systemd::ListScopes => Some(Reply::Hang),
                _ => None,
            });
            let begun = Instant::now();
            let error = sweep(&rig.env).await.err().unwrap();
            assert_eq!(
                begun.elapsed(),
                secs(13),
                "discovery stops two seconds short of the deadline"
            );
            assert!(matches!(
                containment(&error).failures(),
                [CleanupFailure::Observe { .. }]
            ));
        })
        .await;
    });
}

#[test]
fn hung_kills_stay_inside_the_slot_and_every_scope_is_still_attempted() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            for name in ["togi-trial-a", "togi-trial-b", "togi-trial-c"] {
                rig.fake.add_unit(&format!("{name}.scope"));
            }
            rig.fake.on_systemd(|call| match call {
                Systemd::Kill {
                    signal: KillSignal::Kill,
                    ..
                }
                | Systemd::Stop { .. } => Some(Reply::Hang),
                _ => None,
            });
            let begun = Instant::now();
            let error = sweep(&rig.env).await.err().unwrap();
            assert!(begun.elapsed() <= secs(15));
            let kills: Vec<_> = calls(&rig)
                .into_iter()
                .filter(|(_, w)| *w == "SIGKILL")
                .collect();
            assert_eq!(kills.len(), 3);
            assert!(
                calls(&rig).iter().all(|(_, what)| *what != "stop"),
                "a failed kill skips its stop"
            );
            assert!(
                containment(&error)
                    .failures()
                    .iter()
                    .any(|failure| matches!(
                        failure,
                        CleanupFailure::KillScope {
                            signal: "SIGKILL",
                            ..
                        }
                    ))
            );
        })
        .await;
    });
}

#[test]
fn hung_signals_share_the_three_second_grace_and_still_end_in_kill_and_stop() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            for name in ["togi-trial-a", "togi-trial-b", "togi-trial-c"] {
                rig.fake.add_unit(&format!("{name}.scope"));
            }
            rig.fake.on_systemd(|call| match call {
                Systemd::Kill {
                    signal: KillSignal::Cont | KillSignal::Term,
                    ..
                } => Some(Reply::Hang),
                _ => None,
            });
            let begun = Instant::now();
            let error = sweep(&rig.env).await.err().unwrap();
            assert!(begun.elapsed() <= secs(15));
            let all = calls(&rig);
            assert_eq!(all.iter().filter(|(_, w)| *w == "SIGCONT").count(), 3);
            assert_eq!(all.iter().filter(|(_, w)| *w == "SIGTERM").count(), 3);
            assert_eq!(all.iter().filter(|(_, w)| *w == "SIGKILL").count(), 3);
            assert_eq!(all.iter().filter(|(_, w)| *w == "stop").count(), 3);
            let signal_times: Vec<_> = rig
                .fake
                .systemd_calls()
                .iter()
                .filter(|(call, _)| {
                    matches!(
                        call,
                        Systemd::Kill {
                            signal: KillSignal::Cont | KillSignal::Term,
                            ..
                        }
                    )
                })
                .map(|(_, at)| at.duration_since(begun))
                .collect();
            assert!(signal_times.iter().all(|at| *at <= secs(3)));
            assert!(!containment(&error).failures().is_empty());
        })
        .await;
    });
}

#[test]
fn an_uncertain_signal_failure_fails_the_sweep_even_when_everything_ends_up_gone() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            rig.fake.on_systemd(|call| match call {
                Systemd::Kill {
                    signal: KillSignal::Term,
                    ..
                } => Some(Reply::Output {
                    success: false,
                    text: "Access denied".into(),
                }),
                _ => None,
            });
            let error = sweep(&rig.env).await.err().unwrap();
            assert!(matches!(
                containment(&error).failures(),
                [CleanupFailure::KillScope {
                    signal: "SIGTERM",
                    ..
                }]
            ));
            assert!(rig.fake.units().is_empty(), "cleanup still ran to the end");
        })
        .await;
    });
}

#[test]
fn a_scope_that_is_already_gone_is_not_a_failure() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            rig.fake.on_systemd(|call| match call {
                Systemd::Kill { .. } => Some(Reply::Output {
                    success: false,
                    text: "Failed to kill unit: Unit togi-trial-a.scope not loaded.".into(),
                }),
                _ => None,
            });
            let (detail, _stopped) = sweep(&rig.env).await.unwrap();
            assert!(detail.starts_with("removed 1 leftover trial scopes"));
        })
        .await;
    });
}

#[test]
fn a_failing_verification_listing_is_containment() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            let mut listings = 0;
            rig.fake.on_systemd(move |call| match call {
                Systemd::ListScopes => {
                    listings += 1;
                    (listings > 1).then(|| Reply::Output {
                        success: false,
                        text: "bus down".into(),
                    })
                }
                _ => None,
            });
            let error = sweep(&rig.env).await.err().unwrap();
            assert!(matches!(
                containment(&error).failures(),
                [CleanupFailure::Observe { .. }]
            ));
        })
        .await;
    });
}

fn bus_unreachable() -> Reply {
    Reply::Fail(togi_command::Error::Spawn {
        program: "systemctl".into(),
        source: std::io::Error::other("bus unreachable"),
    })
}

fn is_the_command_failure(failure: &CleanupFailure) -> bool {
    std::error::Error::source(failure).is_some_and(|source| {
        matches!(
            source.downcast_ref::<togi_command::Error>(),
            Some(togi_command::Error::Spawn { .. })
        )
    })
}

fn labels(error: &Error) -> Vec<String> {
    containment(error)
        .failures()
        .iter()
        .map(|failure| match failure {
            CleanupFailure::KillScope { signal, .. } => format!("kill {signal}"),
            CleanupFailure::StopScope { .. } => "stop".to_owned(),
            CleanupFailure::Observe { what, .. } => format!("observe {what}"),
            CleanupFailure::Leftover => "leftover".to_owned(),
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

#[test]
fn a_signal_command_that_cannot_run_is_kept_even_when_everything_ends_up_gone() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            rig.fake.on_systemd(|call| match call {
                Systemd::Kill {
                    signal: KillSignal::Term,
                    ..
                } => Some(bus_unreachable()),
                _ => None,
            });
            let error = sweep(&rig.env).await.err().unwrap();
            assert_eq!(labels(&error), ["kill SIGTERM"]);
            assert!(is_the_command_failure(&containment(&error).failures()[0]));
            assert!(rig.fake.units().is_empty(), "cleanup still ran to the end");
        })
        .await;
    });
}

#[test]
fn a_kill_command_that_cannot_run_is_kept_and_its_scope_is_not_stopped() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            rig.fake.on_systemd(|call| match call {
                Systemd::Kill {
                    signal: KillSignal::Kill,
                    ..
                } => Some(bus_unreachable()),
                _ => None,
            });
            let error = sweep(&rig.env).await.err().unwrap();
            assert_eq!(labels(&error), ["kill SIGKILL", "leftover"]);
            assert!(is_the_command_failure(&containment(&error).failures()[0]));
            assert!(
                !calls(&rig).contains(&("togi-trial-a".to_owned(), "stop")),
                "a scope whose kill failed is not stopped"
            );
        })
        .await;
    });
}

#[test]
fn a_stop_command_that_cannot_run_is_kept_and_the_unit_is_a_leftover() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            rig.fake.on_systemd(|call| match call {
                Systemd::Stop { .. } => Some(bus_unreachable()),
                _ => None,
            });
            let error = sweep(&rig.env).await.err().unwrap();
            assert_eq!(labels(&error), ["stop", "leftover"]);
            assert!(is_the_command_failure(&containment(&error).failures()[0]));
        })
        .await;
    });
}

#[test]
fn an_initial_listing_that_cannot_run_is_not_an_empty_machine() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            rig.fake.on_systemd(|call| match call {
                Systemd::ListScopes => Some(bus_unreachable()),
                _ => None,
            });
            let error = sweep(&rig.env).await.err().unwrap();
            assert_eq!(labels(&error), ["observe the trial scope list"]);
            assert!(is_the_command_failure(&containment(&error).failures()[0]));
            assert!(calls(&rig).is_empty());
        })
        .await;
    });
}

#[test]
fn a_final_listing_that_cannot_run_withholds_stopped_even_when_the_machine_is_clean() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            let mut listings = 0;
            rig.fake.on_systemd(move |call| match call {
                Systemd::ListScopes => {
                    listings += 1;
                    (listings > 1).then(bus_unreachable)
                }
                _ => None,
            });
            let error = sweep(&rig.env).await.err().unwrap();
            assert_eq!(labels(&error), ["observe the trial scope list"]);
            assert!(is_the_command_failure(&containment(&error).failures()[0]));
            assert!(rig.fake.units().is_empty());
        })
        .await;
    });
}

#[test]
fn command_failures_keep_every_earlier_failure_in_order() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            let mut listings = 0;
            rig.fake.on_systemd(move |call| match call {
                Systemd::Kill {
                    signal: KillSignal::Term,
                    ..
                }
                | Systemd::Stop { .. } => Some(bus_unreachable()),
                Systemd::ListScopes => {
                    listings += 1;
                    (listings > 1).then(bus_unreachable)
                }
                _ => None,
            });
            let error = sweep(&rig.env).await.err().unwrap();
            assert_eq!(
                labels(&error),
                ["kill SIGTERM", "stop", "observe the trial scope list"]
            );
            assert!(
                containment(&error)
                    .failures()
                    .iter()
                    .all(is_the_command_failure)
            );
        })
        .await;
    });
}

#[test]
fn the_probe_reports_a_confined_scope_and_names_failures() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            assert_eq!(
                probe(&rig.env).await.unwrap(),
                "scope confined to cpu 0 created"
            );
            let probes = rig
                .fake
                .systemd_calls()
                .iter()
                .filter(|(call, _)| matches!(call, Systemd::Probe { .. }))
                .count();
            assert_eq!(probes, 1);

            rig.fake.on_systemd(|call| match call {
                Systemd::Probe { .. } => Some(Reply::Output {
                    success: false,
                    text: String::new(),
                }),
                _ => None,
            });
            assert!(matches!(probe(&rig.env).await, Err(Error::Probe { .. })));
        })
        .await;
    });
}

#[test]
fn the_probe_is_bounded_by_thirty_seconds_and_needs_an_account() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_systemd(|call| match call {
                Systemd::Probe { .. } => Some(Reply::Hang),
                _ => None,
            });
            let begun = Instant::now();
            assert!(matches!(probe(&rig.env).await, Err(Error::Probe { .. })));
            assert_eq!(begun.elapsed(), secs(30));

            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
                env.identity = None;
            });
            assert!(matches!(
                probe(&rig.env).await,
                Err(Error::Identity(crate::IdentityError::NotConfigured))
            ));
            assert!(rig.fake.systemd_calls().is_empty());
        })
        .await;
    });
}

#[test]
fn a_failed_grace_observation_still_kills_and_stops_every_known_scope() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            let _member = rig.fake.add_recovered(process("togi-trial-a", 50));
            rig.fake.fail_alive();
            let error = sweep(&rig.env).await.err().unwrap();
            let all = calls(&rig);
            assert!(all.contains(&("togi-trial-a".to_owned(), "SIGKILL")));
            assert!(all.contains(&("togi-trial-a".to_owned(), "stop")));
            assert!(matches!(
                containment(&error).failures().first(),
                Some(CleanupFailure::Observe {
                    what: "a captured trial process",
                    ..
                })
            ));
        })
        .await;
    });
}

#[test]
fn a_verification_failure_keeps_the_earlier_kill_failure() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            let mut listings = 0;
            rig.fake.on_systemd(move |call| match call {
                Systemd::Kill {
                    signal: KillSignal::Kill,
                    ..
                } => Some(Reply::Output {
                    success: false,
                    text: "Access denied".into(),
                }),
                Systemd::ListScopes => {
                    listings += 1;
                    (listings > 1).then(|| Reply::Output {
                        success: false,
                        text: "bus down".into(),
                    })
                }
                _ => None,
            });
            let error = sweep(&rig.env).await.err().unwrap();
            let failures = containment(&error).failures();
            assert!(matches!(
                failures.first(),
                Some(CleanupFailure::KillScope {
                    signal: "SIGKILL",
                    ..
                })
            ));
            assert!(failures.iter().any(|failure| matches!(
                failure,
                CleanupFailure::Observe {
                    what: "the trial scope list",
                    ..
                }
            )));
        })
        .await;
    });
}

#[test]
fn a_hanging_process_scan_is_cut_short_of_the_deadline_and_listed_scopes_are_still_killed() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            rig.fake.hang_scans();
            let begun = Instant::now();
            let error = sweep(&rig.env).await.err().unwrap();
            assert!(begun.elapsed() <= secs(15), "{:?}", begun.elapsed());
            let all = calls(&rig);
            assert!(all.contains(&("togi-trial-a".to_owned(), "SIGKILL")));
            assert!(all.contains(&("togi-trial-a".to_owned(), "stop")));
            let kill_at = rig
                .fake
                .systemd_calls()
                .iter()
                .find(|(call, _)| {
                    matches!(
                        call,
                        Systemd::Kill {
                            signal: KillSignal::Kill,
                            ..
                        }
                    )
                })
                .map(|(_, at)| at.duration_since(begun))
                .unwrap();
            assert!(kill_at < secs(15), "the kill got a live slot: {kill_at:?}");
            assert!(
                rig.fake.units().is_empty(),
                "the kill and stop took effect, not only got attempted"
            );
            assert!(matches!(
                containment(&error).failures().first(),
                Some(CleanupFailure::Observe {
                    what: "the trial processes",
                    ..
                })
            ));
        })
        .await;
    });
}

#[test]
fn a_stop_that_hangs_or_fails_after_a_successful_kill_is_kept_and_the_other_scopes_still_stop() {
    crate::testing::runtime(true).block_on(async move {
        for hang in [true, false] {
            local(async move {
                let rig = rig();
                for name in ["togi-trial-a", "togi-trial-b", "togi-trial-c"] {
                    rig.fake.add_unit(&format!("{name}.scope"));
                }
                rig.fake.on_systemd(move |call| match call {
                    Systemd::Stop { scope } if scope.to_string() == "togi-trial-a" => {
                        Some(if hang {
                            Reply::Hang
                        } else {
                            Reply::Output {
                                success: false,
                                text: "Access denied".into(),
                            }
                        })
                    }
                    _ => None,
                });
                let begun = Instant::now();
                let error = sweep(&rig.env).await.err().unwrap();
                assert!(begun.elapsed() <= secs(15), "{:?}", begun.elapsed());
                let all = calls(&rig);
                assert_eq!(all.iter().filter(|(_, w)| *w == "SIGKILL").count(), 3);
                assert_eq!(
                    all.iter().filter(|(_, w)| *w == "stop").count(),
                    3,
                    "the other scopes are still stopped"
                );
                let stops = rig
                    .fake
                    .systemd_calls()
                    .iter()
                    .filter(|(call, _)| matches!(call, Systemd::Stop { .. }))
                    .map(|(_, at)| at.duration_since(begun))
                    .collect::<Vec<_>>();
                assert!(stops.iter().all(|at| *at <= secs(2)), "{stops:?}");
                let failures = containment(&error).failures();
                assert!(
                    failures.iter().any(|failure| matches!(
                        failure,
                        CleanupFailure::StopScope { scope, .. } if scope == "togi-trial-a"
                    )),
                    "{failures:?}"
                );
                assert!(
                    !failures
                        .iter()
                        .any(|failure| matches!(failure, CleanupFailure::KillScope { .. })),
                    "the kill itself succeeded: {failures:?}"
                );
                assert_eq!(rig.fake.units(), ["togi-trial-a.scope"]);
            })
            .await;
        }
    });
}

#[test]
fn a_failing_verification_discovery_withholds_stopped_and_keeps_the_earlier_failure() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            let fake = rig.fake.clone();
            let mut listings = 0;
            rig.fake.on_systemd(move |call| match call {
                Systemd::Kill {
                    signal: KillSignal::Term,
                    ..
                } => Some(Reply::Output {
                    success: false,
                    text: "Access denied".into(),
                }),
                Systemd::ListScopes => {
                    listings += 1;
                    if listings > 1 {
                        // The verification's process scan, which follows its listing, now hangs.
                        fake.hang_scans();
                    }
                    None
                }
                _ => None,
            });
            let error = sweep(&rig.env).await.err().unwrap();
            let failures = containment(&error).failures();
            assert!(
                matches!(
                    failures.first(),
                    Some(CleanupFailure::KillScope {
                        signal: "SIGTERM",
                        ..
                    })
                ),
                "{failures:?}"
            );
            assert!(
                failures.iter().any(|failure| matches!(
                    failure,
                    CleanupFailure::Observe {
                        what: "the trial processes",
                        ..
                    }
                )),
                "{failures:?}"
            );
            assert!(
                !failures
                    .iter()
                    .any(|failure| matches!(failure, CleanupFailure::Leftover)),
                "an unobserved machine is not reported as a leftover: {failures:?}"
            );
            assert!(rig.fake.units().is_empty(), "cleanup still ran to the end");
        })
        .await;
    });
}

#[test]
fn cancelling_during_discovery_returns_cancelled_at_once_and_signals_nothing() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            rig.fake.on_systemd(|call| match call {
                Systemd::ListScopes => Some(Reply::Hang),
                _ => None,
            });
            let cancel = CancellationToken::new();
            let trigger = cancel.clone();
            spawn_local(async move {
                sleep(secs(1)).await;
                trigger.cancel();
            });
            let begun = Instant::now();
            let error = run_sweep(&rig.env, &cancel).await.err().unwrap();
            assert!(matches!(error, Error::Cancelled), "{error:?}");
            assert_eq!(begun.elapsed(), secs(1));
            assert!(calls(&rig).is_empty());
        })
        .await;
    });
}

#[test]
fn cancelling_during_initial_process_scan_finishes_listed_scope_cleanup_and_keeps_errors() {
    crate::testing::runtime(true).block_on(async move {
        for fail_stop in [false, true] {
            local(async move {
                let rig = rig();
                rig.fake.add_unit("togi-trial-a.scope");
                rig.fake.hang_scans();
                if fail_stop {
                    rig.fake.on_systemd(|call| match call {
                        Systemd::Stop { .. } => Some(Reply::Output {
                            success: false,
                            text: "Access denied".into(),
                        }),
                        _ => None,
                    });
                }
                let scanning = std::rc::Rc::new(tokio::sync::Notify::new());
                let entered = std::rc::Rc::clone(&scanning);
                rig.fake.on_scan(move || entered.notify_one());
                let cancel = CancellationToken::new();
                let trigger = cancel.clone();
                spawn_local(async move {
                    scanning.notified().await;
                    trigger.cancel();
                });
                let begun = Instant::now();
                let error = run_sweep(&rig.env, &cancel).await.err().unwrap();
                let all = calls(&rig);
                assert!(
                    all.contains(&("togi-trial-a".to_owned(), "SIGKILL")),
                    "{all:?}"
                );
                assert!(
                    all.contains(&("togi-trial-a".to_owned(), "stop")),
                    "{all:?}"
                );
                assert!(begun.elapsed() <= secs(15), "{:?}", begun.elapsed());
                for (operation, at) in rig.fake.systemd_calls() {
                    if matches!(operation, Systemd::Kill { .. } | Systemd::Stop { .. }) {
                        assert!(at.duration_since(begun) < secs(15));
                    }
                }
                let Error::Joined(joined) = &error else {
                    panic!("not joined: {error:?}");
                };
                let [Error::Cancelled, second] = joined.errors() else {
                    panic!("unexpected members: {error:?}");
                };
                let failures = containment(second).failures();
                assert!(matches!(
                    failures.first(),
                    Some(CleanupFailure::Observe {
                        what: "the trial processes",
                        ..
                    })
                ));
                assert_eq!(
                    failures
                        .iter()
                        .any(|failure| matches!(failure, CleanupFailure::StopScope { .. })),
                    fail_stop,
                    "{failures:?}"
                );
                assert_eq!(rig.fake.units().is_empty(), !fail_stop);
            })
            .await;
        }
    });
}

#[test]
fn cancelling_during_initial_process_scan_without_a_known_scope_returns_at_once() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.hang_scans();
            let scanning = std::rc::Rc::new(tokio::sync::Notify::new());
            let entered = std::rc::Rc::clone(&scanning);
            rig.fake.on_scan(move || entered.notify_one());
            let cancel = CancellationToken::new();
            let trigger = cancel.clone();
            spawn_local(async move {
                scanning.notified().await;
                trigger.cancel();
            });
            let begun = Instant::now();
            let error = run_sweep(&rig.env, &cancel).await.err().unwrap();
            assert!(matches!(error, Error::Cancelled), "{error:?}");
            assert_eq!(begun.elapsed(), Duration::ZERO);
            assert!(calls(&rig).is_empty());
        })
        .await;
    });
}

#[test]
fn cancelling_after_the_term_still_kills_and_stops_every_discovered_scope() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            // Nothing but SIGKILL ends this captured process: it ignores TERM.
            let stubborn = rig.fake.add_recovered(process("togi-trial-a", 4242));
            let cancel = CancellationToken::new();
            let trigger = cancel.clone();
            spawn_local(async move {
                sleep(secs(1)).await;
                trigger.cancel();
            });
            let error = run_sweep(&rig.env, &cancel).await.err().unwrap();
            assert!(matches!(error, Error::Cancelled), "{error:?}");
            let all = calls(&rig);
            let term = all.iter().position(|(_, w)| *w == "SIGTERM").unwrap();
            let kill = all.iter().position(|(_, w)| *w == "SIGKILL").unwrap();
            assert!(term < kill && all.contains(&("togi-trial-a".to_owned(), "stop")));
            assert!(!stubborn.alive.get(), "the sweep finished the cleanup");
            assert!(rig.fake.units().is_empty());
        })
        .await;
    });
}

#[test]
fn a_cancelled_sweep_whose_cleanup_fails_reports_cancelled_and_the_containment_failure() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            let _stubborn = rig.fake.add_recovered(process("togi-trial-a", 4242));
            rig.fake.on_systemd(|call| match call {
                Systemd::Kill {
                    signal: KillSignal::Kill,
                    ..
                } => Some(Reply::Output {
                    success: false,
                    text: "Access denied".into(),
                }),
                _ => None,
            });
            let cancel = CancellationToken::new();
            let trigger = cancel.clone();
            spawn_local(async move {
                sleep(secs(1)).await;
                trigger.cancel();
            });
            let error = run_sweep(&rig.env, &cancel).await.err().unwrap();
            let Error::Joined(joined) = &error else {
                panic!("not joined: {error:?}");
            };
            let [Error::Cancelled, second] = joined.errors() else {
                panic!("unexpected members: {error:?}");
            };
            assert!(matches!(
                containment(second).failures().first(),
                Some(CleanupFailure::KillScope {
                    signal: "SIGKILL",
                    ..
                })
            ));
            assert!(
                calls(&rig).iter().any(|(_, what)| *what == "SIGKILL"),
                "the kill was still attempted after the cancel"
            );
        })
        .await;
    });
}

#[cfg(target_os = "linux")]
mod killed_owner {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use togi_machine::{CancellationToken, Trials};
    use tokio::time::Instant;

    use super::super::process::{Escaped, Helper, helper_command, linux, runner_over, timing};
    use crate::id::ScopeName;
    use crate::launch::LaunchCommand;
    use crate::os::linux::{Leader, Linux};
    use crate::os::{Child as _, ExitInfo, GroupSignal, KillSignal, LaunchFailure, Os, Systemd};
    use crate::procfs::{
        Proc, ProcError, ScopeProcess, Threads, Usage, parse_stat, ticks_per_second,
    };
    use crate::timing::Timing;

    struct Leftovers {
        listed: Mutex<bool>,
        operations: Mutex<Vec<(String, &'static str)>>,
        orphan: ScopeProcess,
    }

    fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Systemd is faked, with one leftover unit whose only member is the real detached
    /// descendant: a scope signal reaches that process, and a stop removes the unit.
    struct Leftover {
        linux: Linux,
        proc: Proc,
        state: Arc<Leftovers>,
    }

    impl Leftover {
        fn orphan_alive(&self) -> Result<bool, ProcError> {
            self.proc.alive(&self.state.orphan)
        }
    }

    impl Os for Leftover {
        type Child = Leader;

        fn launch(&self, _: &LaunchCommand) -> Result<Leader, LaunchFailure<Leader>> {
            Err(std::io::Error::other("the sweep launches nothing").into())
        }

        fn in_scope(&self, pid: u32, scope: &ScopeName) -> Result<bool, ProcError> {
            self.linux.in_scope(pid, scope)
        }

        fn usage(&self, pid: u32) -> Result<Usage, ProcError> {
            self.linux.usage(pid)
        }

        fn threads(&self, pid: u32) -> Result<Threads, ProcError> {
            self.linux.threads(pid)
        }

        #[expect(
            clippy::unused_async_trait_impl,
            reason = "the trait's operation is async; this seam answers at once"
        )]
        async fn systemd(
            &self,
            operation: &Systemd,
            _: Instant,
        ) -> Result<togi_command::Output, togi_command::Error> {
            use std::os::unix::process::ExitStatusExt as _;
            let stdout = match operation {
                Systemd::ListScopes if *locked(&self.state.listed) => {
                    format!(
                        "{} loaded active running x\n",
                        self.state.orphan.scope.unit()
                    )
                }
                Systemd::Kill { scope, signal } => {
                    let name = match signal {
                        KillSignal::Cont => "SIGCONT",
                        KillSignal::Term => "SIGTERM",
                        KillSignal::Kill => "SIGKILL",
                    };
                    locked(&self.state.operations).push((scope.to_string(), name));
                    let delivered = match signal {
                        KillSignal::Cont => rustix::process::Signal::CONT,
                        KillSignal::Term => rustix::process::Signal::TERM,
                        KillSignal::Kill => rustix::process::Signal::KILL,
                    };
                    if *scope == self.state.orphan.scope
                        && self.orphan_alive().unwrap_or(false)
                        && let Some(pid) = i32::try_from(self.state.orphan.pid)
                            .ok()
                            .and_then(rustix::process::Pid::from_raw)
                    {
                        let _ = rustix::process::kill_process(pid, delivered);
                    }
                    String::new()
                }
                Systemd::Stop { scope } => {
                    locked(&self.state.operations).push((scope.to_string(), "stop"));
                    *locked(&self.state.listed) = false;
                    String::new()
                }
                Systemd::ListScopes | Systemd::Probe { .. } => String::new(),
            };
            Ok(togi_command::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: stdout.into_bytes(),
                stderr: Vec::new(),
            })
        }

        #[expect(
            clippy::unused_async_trait_impl,
            reason = "the trait's operation is async; this seam answers at once"
        )]
        async fn scope_processes(&self, _: Instant) -> Result<Vec<ScopeProcess>, ProcError> {
            Ok(if self.orphan_alive()? {
                vec![self.state.orphan.clone()]
            } else {
                Vec::new()
            })
        }

        async fn survivors(
            &self,
            captured: &[ScopeProcess],
            deadline: Instant,
        ) -> Result<usize, ProcError> {
            self.linux.survivors(captured, deadline).await
        }
    }

    fn poll_until(end: std::time::Instant, mut ready: impl FnMut() -> bool) -> bool {
        loop {
            if ready() {
                return true;
            }
            if std::time::Instant::now() >= end {
                return false;
            }
            std::thread::yield_now();
        }
    }

    fn parent_of(pid: u32) -> Option<u32> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        text[text.rfind(')')? + 1..]
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Freeze {
        Frozen,
        Gone,
        Running,
        Unobserved(String),
    }

    fn vanished(error: &std::io::Error) -> bool {
        error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(3)
    }

    fn read_stat(
        inputs: &crate::procfs::Inputs,
        path: &Path,
    ) -> Result<Option<crate::procfs::Stat>, String> {
        match inputs.read_to_string(path) {
            Ok(text) => parse_stat(&text)
                .map(Some)
                .map_err(|error| format!("{}: {error}", path.display())),
            Err(error) if vanished(&error) => Ok(None),
            Err(error) => Err(format!("read {}: {error}", path.display())),
        }
    }

    fn freeze_of(inputs: &crate::procfs::Inputs, root: &Path, pid: u32) -> Freeze {
        let owner = root.join(pid.to_string());
        match read_stat(inputs, &owner.join("stat")) {
            Ok(None) => return Freeze::Gone,
            Ok(Some(stat)) if matches!(stat.state, 'Z' | 'X') => return Freeze::Gone,
            Ok(Some(_)) => {}
            Err(reason) => return Freeze::Unobserved(reason),
        }
        let tasks = match inputs.read_dir(&owner.join("task")) {
            Ok(tasks) => tasks,
            Err(error) if vanished(&error) => return Freeze::Gone,
            Err(error) => return Freeze::Unobserved(format!("list tasks of {pid}: {error}")),
        };
        let mut seen = 0;
        let mut running = false;
        let mut unobserved = None;
        for task in tasks {
            let task = match task {
                Ok(task) => task,
                Err(error) => {
                    unobserved.get_or_insert(format!("list tasks of {pid}: {error}"));
                    continue;
                }
            };
            match read_stat(inputs, &task.path().join("stat")) {
                Ok(None) => {}
                Ok(Some(stat)) => {
                    seen += 1;
                    running |= !matches!(stat.state, 'T' | 't');
                }
                Err(reason) => {
                    unobserved.get_or_insert(reason);
                }
            }
        }
        if running {
            Freeze::Running
        } else if let Some(reason) = unobserved {
            Freeze::Unobserved(reason)
        } else if seen == 0 {
            Freeze::Unobserved(format!("no thread of {pid} observed"))
        } else {
            Freeze::Frozen
        }
    }

    trait Subject {
        fn stop(&self) -> bool;
        fn freeze(&self) -> Freeze;
        fn children(&mut self) -> Option<Vec<u32>>;
        fn kill_child(&self, pid: u32);
        fn child_alive(&self, pid: u32) -> bool;
        fn kill_owner(&self);
        fn reap_owner(&mut self) -> bool;
        fn settle(&self, ready: &mut dyn FnMut() -> bool) -> bool;
    }

    const QUIET_ROUNDS: u32 = 2;
    const TRACKING_ROUNDS: u32 = 50;

    fn contain(subject: &mut impl Subject) -> Vec<String> {
        let mut problems = Vec::new();
        let stopped = subject.stop();
        let mut last = Freeze::Running;
        let confirmed = stopped
            && subject.settle(&mut || {
                last = subject.freeze();
                matches!(last, Freeze::Frozen | Freeze::Gone)
            });
        if !stopped {
            problems.push("the owner could not be stopped".to_owned());
        } else if !confirmed {
            problems.push(format!(
                "the owner was not confirmed frozen; last observation {last:?}"
            ));
        }
        let rounds = if confirmed { 1 } else { TRACKING_ROUNDS };
        let mut known: Vec<u32> = Vec::new();
        let mut quiet = 0;
        let mut settled = false;
        for _ in 0..rounds {
            let Some(found) = subject.children() else {
                quiet = 0;
                continue;
            };
            let fresh: Vec<u32> = found
                .into_iter()
                .filter(|pid| !known.contains(pid))
                .collect();
            for pid in &fresh {
                subject.kill_child(*pid);
            }
            quiet = if fresh.is_empty() { quiet + 1 } else { 0 };
            known.extend(fresh);
            if confirmed || quiet >= QUIET_ROUNDS {
                settled = true;
                break;
            }
        }
        if !settled {
            problems.push(format!(
                "descendants of the owner were still appearing or unobservable after {rounds} rounds"
            ));
        }
        subject.kill_owner();
        if !subject.reap_owner() {
            problems.push("the owner was not reaped".to_owned());
        }
        let dead = subject.settle(&mut || known.iter().all(|pid| !subject.child_alive(*pid)));
        if !dead {
            problems.push("a descendant survived its kill".to_owned());
        }
        problems
    }

    fn fail_loudly(problems: &[String]) {
        if problems.is_empty() {
            return;
        }
        let message = format!("owner containment unconfirmed: {}", problems.join("; "));
        if std::thread::panicking() {
            eprintln!("{message}");
        } else {
            panic!("{message}");
        }
    }

    struct RealOwner {
        leader: Option<Leader>,
        pinned: BTreeMap<u32, Escaped>,
    }

    impl RealOwner {
        fn new(leader: Leader) -> Self {
            Self {
                leader: Some(leader),
                pinned: BTreeMap::new(),
            }
        }

        fn leader(&self) -> &Leader {
            self.leader
                .as_ref()
                .expect("the owner is held until it is reaped")
        }
    }

    fn child_pids(inputs: &crate::procfs::Inputs, root: &Path, parent: u32) -> Option<Vec<u32>> {
        let mut found = Vec::new();
        for entry in inputs.read_dir(root).ok()? {
            let Some(pid) = entry
                .ok()?
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let text = match inputs.read_to_string(&root.join(pid.to_string()).join("stat")) {
                Ok(text) => text,
                Err(error) if vanished(&error) => continue,
                Err(_) => return None,
            };
            let ppid: u32 = text[text.rfind(')')? + 1..]
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()?;
            if ppid == parent {
                found.push(pid);
            }
        }
        Some(found)
    }

    impl Subject for RealOwner {
        fn stop(&self) -> bool {
            self.leader().signal_group(GroupSignal::Stop).is_ok()
        }

        fn freeze(&self) -> Freeze {
            freeze_of(
                &crate::procfs::Inputs::default(),
                Path::new("/proc"),
                self.leader().pid(),
            )
        }

        fn children(&mut self) -> Option<Vec<u32>> {
            let found = child_pids(
                &crate::procfs::Inputs::default(),
                Path::new("/proc"),
                self.leader().pid(),
            )?;
            for pid in &found {
                if !self.pinned.contains_key(pid)
                    && let Some(child) = Escaped::adopt(*pid)
                {
                    self.pinned.insert(*pid, child);
                }
            }
            Some(found)
        }

        fn kill_child(&self, pid: u32) {
            if let Some(child) = self.pinned.get(&pid) {
                child.kill();
            }
        }

        fn child_alive(&self, pid: u32) -> bool {
            self.pinned.get(&pid).is_some_and(Escaped::alive)
        }

        fn kill_owner(&self) {
            let _ = self.leader().signal_group(GroupSignal::Kill);
        }

        fn reap_owner(&mut self) -> bool {
            let end = std::time::Instant::now() + Duration::from_secs(5);
            let exited = poll_until(end, || {
                self.leader().exit().is_ok_and(|exit| exit.is_some())
            });
            exited
                && self
                    .leader
                    .take()
                    .is_some_and(|leader| leader.reap().is_ok())
        }

        fn settle(&self, ready: &mut dyn FnMut() -> bool) -> bool {
            poll_until(std::time::Instant::now() + Duration::from_secs(5), ready)
        }
    }

    /// Owns the owner from launch until its observed exit and reap. Dropped while still holding
    /// it, it freezes the owner and waits until every thread is stopped, so no further fork can
    /// race the enumeration of its children. When the freeze is not confirmed, it tracks and
    /// kills descendants for a bounded number of rounds until none new appears. Anything it
    /// could not confirm panics. It spawns nothing and signals no pid it did not pin or hold
    /// unreaped.
    struct Armed<S: Subject>(Option<S>);

    type OwnerGuard = Armed<RealOwner>;

    impl OwnerGuard {
        fn owner(&self) -> &Leader {
            self.0
                .as_ref()
                .expect("the owner is held until it is reaped")
                .leader()
        }

        fn kill_and_reap(&mut self) -> ExitInfo {
            let owner = self.owner();
            assert!(owner.signal_group(GroupSignal::Kill).is_ok());
            assert!(poll_until(
                std::time::Instant::now() + Duration::from_secs(10),
                || owner.exit().is_ok_and(|exit| exit.is_some())
            ));
            self.0
                .take()
                .and_then(|owner| owner.leader)
                .expect("held above")
                .reap()
                .unwrap()
        }
    }

    impl<S: Subject> Drop for Armed<S> {
        fn drop(&mut self) {
            if let Some(mut subject) = self.0.take() {
                fail_loudly(&contain(&mut subject));
            }
        }
    }

    fn setup_owner(
        os: &Linux,
        dir: &std::path::Path,
        after_launch: impl FnOnce(&Leader),
    ) -> (Escaped, ScopeProcess) {
        let mut guard = Armed(Some(RealOwner::new(
            os.launch(&helper_command("scope-owner", dir)).unwrap(),
        )));
        after_launch(guard.owner());
        let published = dir.join("descendant.pid");
        assert!(
            poll_until(std::time::Instant::now() + Duration::from_secs(10), || {
                published.exists()
            }),
            "the owner started no detached descendant"
        );
        let pid: u32 = std::fs::read_to_string(&published)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            parent_of(pid),
            Some(guard.owner().pid()),
            "the published process is the owner's child"
        );
        let descendant = Escaped::adopt(pid).expect("the descendant is alive");
        assert_eq!(guard.kill_and_reap(), ExitInfo::Signal(9));
        let starttime = stat_of(pid)
            .map(|stat| stat.starttime)
            .expect("the descendant outlived its owner");
        let orphan = ScopeProcess {
            scope: ScopeName::from_unit("togi-trial-leftover.scope").unwrap(),
            pid,
            starttime,
        };
        (descendant, orphan)
    }

    struct SetupWitness {
        dir: std::path::PathBuf,
        owner: Option<(u32, u64)>,
    }

    fn stat_of(pid: u32) -> Option<crate::procfs::Stat> {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|text| parse_stat(&text).ok())
    }

    impl SetupWitness {
        fn new(dir: &std::path::Path) -> Self {
            Self {
                dir: dir.to_owned(),
                owner: None,
            }
        }

        fn note_owner(&mut self, owner: &Leader) {
            self.owner = stat_of(owner.pid()).map(|stat| (owner.pid(), stat.starttime));
        }

        fn live_in_dir(&self) -> Vec<Escaped> {
            let Ok(entries) = std::fs::read_dir("/proc") else {
                return Vec::new();
            };
            entries
                .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
                .filter(|pid| {
                    std::fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd == self.dir)
                })
                .filter_map(Escaped::adopt)
                .filter(Escaped::alive)
                .collect()
        }

        fn owner_unreaped(&self) -> bool {
            self.owner.is_some_and(|(pid, starttime)| {
                stat_of(pid).is_some_and(|stat| stat.starttime == starttime)
            })
        }
    }

    impl Drop for SetupWitness {
        fn drop(&mut self) {
            let end = std::time::Instant::now() + Duration::from_secs(5);
            let leaked = self.live_in_dir();
            for process in &leaked {
                process.kill();
            }
            let _ = poll_until(end, || leaked.iter().all(|process| !process.alive()));
            if self.owner_unreaped()
                && let Some((pid, _)) = self.owner
                && let Some(pid) = i32::try_from(pid)
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::NOHANG);
            }
        }
    }

    #[test]
    fn real_process_owner_setup_failures_leave_no_owner_or_detached_descendant() {
        for fault in ["immediately", "before-publication-read", "read", "parse"] {
            let dir = crate::testing::tempdir();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _enter = runtime.enter();
            let os = linux();
            let mut witness = SetupWitness::new(dir.path());
            let begun = std::time::Instant::now();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                setup_owner(&os, dir.path(), |owner| {
                    witness.note_owner(owner);
                    assert!(fault != "immediately", "injected setup failure");
                    let published = dir.path().join("descendant.pid");
                    assert!(poll_until(
                        std::time::Instant::now() + Duration::from_secs(10),
                        || published.exists()
                    ));
                    std::fs::rename(&published, dir.path().join("descendant.pid.tmp")).unwrap();
                    match fault {
                        "before-publication-read" => panic!("injected setup failure"),
                        "read" => std::fs::create_dir(&published).unwrap(),
                        "parse" => std::fs::write(&published, "invalid pid").unwrap(),
                        _ => unreachable!(),
                    }
                })
            }));
            let leaked: Vec<u32> = witness.live_in_dir().iter().map(Escaped::pid).collect();
            let unreaped = witness.owner_unreaped();
            drop(witness);
            assert!(result.is_err(), "{fault}");
            assert!(
                leaked.is_empty(),
                "processes leaked after {fault}: {leaked:?}"
            );
            assert!(!unreaped, "the owner was not reaped after {fault}");
            assert!(begun.elapsed() < Duration::from_secs(15), "{fault}");
        }
    }

    /// Go `TestProcessSweepAfterKilledOwner`: a real owner starts a detached descendant and is
    /// killed; the descendant survives, and the sweep removes it.
    #[test]
    fn real_process_a_sweep_after_a_killed_owner_removes_its_detached_descendant() {
        let dir = crate::testing::tempdir();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let os = linux();
        let (descendant, orphan) = {
            let _enter = runtime.enter();
            setup_owner(&os, dir.path(), |_| {})
        };
        assert!(
            descendant.alive(),
            "the detached descendant survived the killed owner"
        );

        let state = Arc::new(Leftovers {
            listed: Mutex::new(true),
            operations: Mutex::new(Vec::new()),
            orphan,
        });
        let make = {
            let state = Arc::clone(&state);
            move || Leftover {
                linux: linux(),
                proc: Proc::new("/proc".into(), ticks_per_second()),
                state,
            }
        };
        let sweep_timing = Timing {
            teardown: Duration::from_secs(10),
            stop_grace: Duration::from_secs(2),
            ..timing()
        };
        let mut runner = runner_over(
            Helper::new("hold", false),
            false,
            sweep_timing,
            dir.path(),
            make,
        );
        let begun = std::time::Instant::now();
        let swept = runner.sweep(&CancellationToken::new()).unwrap();
        assert!(begun.elapsed() < sweep_timing.teardown + Duration::from_secs(1));
        assert_eq!(
            swept.detail,
            "removed 1 leftover trial scopes; no trial unit or process remains"
        );
        assert!(
            !descendant.alive(),
            "the sweep removed the detached descendant"
        );
        let operations = locked(&state.operations);
        assert!(
            operations.contains(&("togi-trial-leftover".to_owned(), "SIGKILL")),
            "{operations:?}"
        );
        assert!(
            operations.contains(&("togi-trial-leftover".to_owned(), "stop")),
            "{operations:?}"
        );
        assert!(!*locked(&state.listed), "the unit was stopped");
    }

    fn stat_text(state: char, parent: u32) -> String {
        format!("1 (x) {state} {parent}{}", " 0".repeat(35))
    }

    fn put(rig: &super::super::harness::Rig, relative: &str, text: &str) {
        rig.write(rig.dir.path().join(relative), text).unwrap();
    }

    fn owner_with_threads(rig: &super::super::harness::Rig, owner: char, threads: &[char]) {
        put(rig, "7/stat", &stat_text(owner, 1));
        rig.create_dir_all(rig.dir.path().join("7/task")).unwrap();
        for (index, state) in threads.iter().enumerate() {
            put(
                rig,
                &format!("7/task/{}/stat", 7 + index),
                &stat_text(*state, 1),
            );
        }
    }

    #[test]
    fn freeze_needs_every_thread_stopped() {
        let rig = super::super::harness::rig();
        owner_with_threads(&rig, 'T', &['T', 't']);
        assert_eq!(
            freeze_of(&rig.env.inputs, rig.dir.path(), 7),
            Freeze::Frozen
        );
        put(&rig, "7/task/9/stat", &stat_text('S', 1));
        assert_eq!(
            freeze_of(&rig.env.inputs, rig.dir.path(), 7),
            Freeze::Running
        );
    }

    #[test]
    fn freeze_treats_only_a_missing_or_dead_owner_as_gone() {
        use togi_journal::fs::Fs as _;
        let rig = super::super::harness::rig();
        assert_eq!(freeze_of(&rig.env.inputs, rig.dir.path(), 7), Freeze::Gone);
        owner_with_threads(&rig, 'Z', &[]);
        assert_eq!(freeze_of(&rig.env.inputs, rig.dir.path(), 7), Freeze::Gone);
        rig.fs.remove_dir(&rig.dir.path().join("7/task")).unwrap();
        put(&rig, "7/stat", &stat_text('T', 1));
        assert_eq!(freeze_of(&rig.env.inputs, rig.dir.path(), 7), Freeze::Gone);
    }

    #[test]
    fn a_failed_owner_stat_read_is_not_gone() {
        use togi_journal::fs::Fs as _;
        let rig = super::super::harness::rig();
        rig.create_dir_all(rig.dir.path().join("7/stat")).unwrap();
        assert!(matches!(
            freeze_of(&rig.env.inputs, rig.dir.path(), 7),
            Freeze::Unobserved(_)
        ));
        rig.fs.remove_dir(&rig.dir.path().join("7/stat")).unwrap();
        put(&rig, "7/stat", "garbled");
        assert!(matches!(
            freeze_of(&rig.env.inputs, rig.dir.path(), 7),
            Freeze::Unobserved(_)
        ));
    }

    #[test]
    fn a_failed_thread_stat_read_never_counts_as_frozen() {
        use togi_journal::fs::Fs as _;
        let rig = super::super::harness::rig();
        owner_with_threads(&rig, 'T', &['T']);
        rig.create_dir_all(rig.dir.path().join("7/task/9/stat"))
            .unwrap();
        assert!(matches!(
            freeze_of(&rig.env.inputs, rig.dir.path(), 7),
            Freeze::Unobserved(_)
        ));
        rig.fs
            .remove_dir(&rig.dir.path().join("7/task/9/stat"))
            .unwrap();
        put(&rig, "7/task/9/stat", "garbled");
        assert!(matches!(
            freeze_of(&rig.env.inputs, rig.dir.path(), 7),
            Freeze::Unobserved(_)
        ));
    }

    #[test]
    fn an_owner_with_no_observed_thread_is_not_frozen() {
        let rig = super::super::harness::rig();
        owner_with_threads(&rig, 'T', &[]);
        assert!(matches!(
            freeze_of(&rig.env.inputs, rig.dir.path(), 7),
            Freeze::Unobserved(_)
        ));
    }

    #[test]
    fn an_unreadable_process_entry_makes_the_child_listing_unobservable() {
        let rig = super::super::harness::rig();
        put(&rig, "10/stat", &stat_text('S', 5));
        put(&rig, "11/stat", &stat_text('S', 6));
        assert_eq!(
            child_pids(&rig.env.inputs, rig.dir.path(), 5),
            Some(vec![10])
        );
        rig.create_dir_all(rig.dir.path().join("12/stat")).unwrap();
        assert_eq!(child_pids(&rig.env.inputs, rig.dir.path(), 5), None);
    }

    struct Fake {
        stop_ok: bool,
        freeze: Freeze,
        visible: fn(usize) -> Vec<u32>,
        listing_lost: bool,
        unkillable: Vec<u32>,
        reaps: bool,
        enumerations: usize,
        freezes: Cell<usize>,
        killed: RefCell<Vec<u32>>,
        owner_killed_after: Cell<Option<usize>>,
    }

    fn fake(freeze: Freeze, visible: fn(usize) -> Vec<u32>) -> Fake {
        Fake {
            stop_ok: true,
            freeze,
            visible,
            listing_lost: false,
            unkillable: Vec::new(),
            reaps: true,
            enumerations: 0,
            freezes: Cell::new(0),
            killed: RefCell::new(Vec::new()),
            owner_killed_after: Cell::new(None),
        }
    }

    fn unconfirmed() -> Freeze {
        Freeze::Unobserved("read failed".to_owned())
    }

    fn late_child(call: usize) -> Vec<u32> {
        if call == 1 { vec![1] } else { vec![1, 2] }
    }

    fn endless_children(call: usize) -> Vec<u32> {
        (1..=u32::try_from(call).unwrap()).collect()
    }

    impl Subject for Fake {
        fn stop(&self) -> bool {
            self.stop_ok
        }

        fn freeze(&self) -> Freeze {
            self.freezes.set(self.freezes.get() + 1);
            self.freeze.clone()
        }

        fn children(&mut self) -> Option<Vec<u32>> {
            self.enumerations += 1;
            (!self.listing_lost).then(|| (self.visible)(self.enumerations))
        }

        fn kill_child(&self, pid: u32) {
            self.killed.borrow_mut().push(pid);
        }

        fn child_alive(&self, pid: u32) -> bool {
            self.unkillable.contains(&pid) || !self.killed.borrow().contains(&pid)
        }

        fn kill_owner(&self) {
            self.owner_killed_after.set(Some(self.enumerations));
        }

        fn reap_owner(&mut self) -> bool {
            self.reaps
        }

        fn settle(&self, ready: &mut dyn FnMut() -> bool) -> bool {
            (0..3).any(|_| ready())
        }
    }

    #[test]
    fn an_unconfirmed_freeze_tracks_late_descendants_until_quiet_and_reports() {
        let mut subject = fake(unconfirmed(), late_child);
        let problems = contain(&mut subject);
        assert_eq!(
            problems,
            ["the owner was not confirmed frozen; last observation Unobserved(\"read failed\")"]
        );
        assert_eq!(*subject.killed.borrow(), [1, 2]);
        assert_eq!(subject.enumerations, 4);
        assert_eq!(subject.owner_killed_after.get(), Some(4));
    }

    #[test]
    fn a_failed_stop_is_reported_and_descendants_are_still_tracked() {
        let mut subject = fake(Freeze::Frozen, late_child);
        subject.stop_ok = false;
        let problems = contain(&mut subject);
        assert_eq!(problems, ["the owner could not be stopped"]);
        assert_eq!(subject.freezes.get(), 0);
        assert_eq!(*subject.killed.borrow(), [1, 2]);
    }

    #[test]
    fn descendants_that_never_stop_appearing_are_bounded_and_reported() {
        let mut subject = fake(unconfirmed(), endless_children);
        let problems = contain(&mut subject);
        assert_eq!(
            subject.enumerations,
            usize::try_from(TRACKING_ROUNDS).unwrap()
        );
        assert_eq!(subject.owner_killed_after.get(), Some(subject.enumerations));
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("still appearing or unobservable after 50 rounds")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_confirmed_freeze_enumerates_once_and_reports_nothing() {
        let mut subject = fake(Freeze::Frozen, |_| vec![1]);
        assert!(contain(&mut subject).is_empty());
        assert_eq!(subject.enumerations, 1);
        assert_eq!(subject.freezes.get(), 1);
        assert_eq!(*subject.killed.borrow(), [1]);
    }

    #[test]
    fn an_unobservable_child_listing_is_not_an_empty_one() {
        let mut subject = fake(Freeze::Frozen, |_| Vec::new());
        subject.listing_lost = true;
        let problems = contain(&mut subject);
        assert_eq!(
            problems,
            ["descendants of the owner were still appearing or unobservable after 1 rounds"]
        );
    }

    #[test]
    fn an_unreaped_owner_and_a_surviving_descendant_are_reported() {
        let mut subject = fake(Freeze::Frozen, |_| vec![1]);
        subject.unkillable = vec![1];
        subject.reaps = false;
        assert_eq!(
            contain(&mut subject),
            ["the owner was not reaped", "a descendant survived its kill"]
        );
    }

    #[test]
    fn dropping_an_armed_guard_panics_on_anything_unconfirmed_and_not_otherwise() {
        let unconfirmed_guard = Armed(Some(fake(unconfirmed(), late_child)));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(unconfirmed_guard);
        }));
        let payload = outcome.expect_err("an unconfirmed freeze must be loud");
        let message = payload.downcast_ref::<String>().expect("a formatted panic");
        assert!(
            message.contains("owner containment unconfirmed"),
            "{message}"
        );
        let clean = Armed(Some(fake(Freeze::Frozen, |_| Vec::new())));
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(clean))).is_ok());
    }
}

#[test]
fn a_captured_process_that_outlives_the_term_holds_the_kill_back_for_the_whole_grace() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            let _stubborn = rig.fake.add_recovered(process("togi-trial-b", 4242));
            let begun = Instant::now();
            let (_detail, _stopped) = sweep(&rig.env).await.unwrap();
            let at = |wanted: KillSignal| -> Vec<Duration> {
                rig.fake
            .systemd_calls()
            .iter()
            .filter(
                |(call, _)| matches!(call, Systemd::Kill { signal, .. } if *signal == wanted),
            )
            .map(|(_, at)| at.duration_since(begun))
            .collect()
            };
            assert_eq!(at(KillSignal::Cont), [Duration::ZERO; 2]);
            assert_eq!(at(KillSignal::Term), [Duration::ZERO; 2]);
            assert_eq!(at(KillSignal::Kill), [secs(3); 2]);
        })
        .await;
    });
}

#[test]
fn a_failed_initial_process_scan_is_containment_and_the_listed_scopes_are_still_killed() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.add_unit("togi-trial-a.scope");
            rig.fake.fault_scans(ScanFault::Fail);
            let error = sweep(&rig.env).await.err().unwrap();
            let failures = containment(&error).failures();
            assert!(
                matches!(
                    failures.first(),
                    Some(failure @ CleanupFailure::Observe { what: "the trial processes", .. })
                        if std::error::Error::source(failure)
                            .is_some_and(|source| source.to_string() == "scan failed")
                ),
                "{failures:?}"
            );
            let all = calls(&rig);
            assert!(
                all.contains(&("togi-trial-a".to_owned(), "SIGKILL")),
                "{all:?}"
            );
            assert!(
                all.contains(&("togi-trial-a".to_owned(), "stop")),
                "{all:?}"
            );
            assert!(rig.fake.units().is_empty());
        })
        .await;
    });
}

#[test]
fn a_hung_verification_listing_is_containment_inside_the_shared_deadline() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            for name in ["togi-trial-a", "togi-trial-b", "togi-trial-c"] {
                rig.fake.add_unit(&format!("{name}.scope"));
            }
            let mut listings = 0;
            rig.fake.on_systemd(move |call| match call {
                Systemd::ListScopes => {
                    listings += 1;
                    (listings > 1).then_some(Reply::Hang)
                }
                _ => None,
            });
            let begun = Instant::now();
            let error = sweep(&rig.env).await.err().unwrap();
            assert!(begun.elapsed() <= secs(15), "{:?}", begun.elapsed());
            let failures = containment(&error).failures();
            assert!(
                matches!(
                    failures,
                    [CleanupFailure::Observe {
                        what: "the trial scope list",
                        ..
                    }]
                ),
                "{failures:?}"
            );
            let all = calls(&rig);
            assert_eq!(all.iter().filter(|(_, w)| *w == "SIGKILL").count(), 3);
            assert_eq!(all.iter().filter(|(_, w)| *w == "stop").count(), 3);
        })
        .await;
    });
}

#[test_case::test_case(KillSignal::Cont; "only the continue signals hang")]
#[test_case::test_case(KillSignal::Term; "only the terminate signals hang")]
fn a_hung_signal_of_either_kind_is_containment_inside_the_grace_and_ends_in_kill_and_stop(
    hung: KillSignal,
) {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            for name in ["togi-trial-a", "togi-trial-b", "togi-trial-c"] {
                rig.fake.add_unit(&format!("{name}.scope"));
            }
            rig.fake.on_systemd(move |call| match call {
                Systemd::Kill { signal, .. } if *signal == hung => Some(Reply::Hang),
                _ => None,
            });
            let begun = Instant::now();
            let error = sweep(&rig.env).await.err().unwrap();
            assert_eq!(begun.elapsed(), rig.env.timing.stop_grace);
            let all = calls(&rig);
            for signal in ["SIGCONT", "SIGTERM", "SIGKILL"] {
                assert_eq!(
                    all.iter().filter(|(_, w)| *w == signal).count(),
                    3,
                    "{signal}"
                );
            }
            assert_eq!(all.iter().filter(|(_, w)| *w == "stop").count(), 3);
            for (call, at) in rig.fake.systemd_calls() {
                if matches!(
                    call,
                    Systemd::Kill {
                        signal: KillSignal::Kill,
                        ..
                    }
                ) {
                    assert_eq!(at.duration_since(begun), rig.env.timing.stop_grace);
                }
            }
            let failures = containment(&error).failures();
            assert_eq!(
                failures
                    .iter()
                    .filter(|failure| matches!(
                        failure,
                        CleanupFailure::KillScope { signal, .. } if *signal == hung.name()
                    ))
                    .count(),
                3,
                "{failures:?}"
            );
            assert!(rig.fake.units().is_empty(), "cleanup still ran to the end");
        })
        .await;
    });
}
