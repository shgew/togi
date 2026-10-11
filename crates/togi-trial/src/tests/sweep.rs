//! The leftover sweep and the preflight probe on the fake OS under paused time.

use std::time::Duration;

use togi_machine::{CancellationToken, Stopped};
use tokio::task::spawn_local;
use tokio::time::{Instant, sleep};

use super::harness::{local, rig, rig_edit};
use crate::error::{CleanupFailure, Error};
use crate::id::ScopeName;
use crate::os::fake::Reply;
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

#[tokio::test(start_paused = true)]
async fn the_sweep_signals_each_exact_scope_in_order_and_removes_them_all() {
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
}

#[tokio::test(start_paused = true)]
async fn a_clean_machine_sweeps_to_success_without_signalling_anything() {
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
}

#[tokio::test(start_paused = true)]
async fn a_lingering_unit_or_a_process_moved_out_of_its_scope_is_containment_after_the_verify_slot()
{
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
}

#[tokio::test(start_paused = true)]
async fn a_hung_initial_listing_is_containment_at_the_shared_deadline() {
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
}

#[tokio::test(start_paused = true)]
async fn hung_kills_stay_inside_the_slot_and_every_scope_is_still_attempted() {
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
}

#[tokio::test(start_paused = true)]
async fn hung_signals_share_the_three_second_grace_and_still_end_in_kill_and_stop() {
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
}

#[tokio::test(start_paused = true)]
async fn an_uncertain_signal_failure_fails_the_sweep_even_when_everything_ends_up_gone() {
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
}

#[tokio::test(start_paused = true)]
async fn a_scope_that_is_already_gone_is_not_a_failure() {
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
}

#[tokio::test(start_paused = true)]
async fn a_failing_verification_listing_is_containment() {
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
}

#[tokio::test(start_paused = true)]
async fn the_probe_reports_a_confined_scope_and_names_failures() {
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
}

#[tokio::test(start_paused = true)]
async fn the_probe_is_bounded_by_thirty_seconds_and_needs_an_account() {
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
}

#[tokio::test(start_paused = true)]
async fn a_failed_grace_observation_still_kills_and_stops_every_known_scope() {
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
}

#[tokio::test(start_paused = true)]
async fn a_verification_failure_keeps_the_earlier_kill_failure() {
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
}

#[tokio::test(start_paused = true)]
async fn a_hanging_process_scan_is_cut_short_of_the_deadline_and_listed_scopes_are_still_killed() {
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
}

#[tokio::test(start_paused = true)]
async fn a_stop_that_hangs_or_fails_after_a_successful_kill_is_kept_and_the_other_scopes_still_stop()
 {
    for hang in [true, false] {
        local(async move {
            let rig = rig();
            for name in ["togi-trial-a", "togi-trial-b", "togi-trial-c"] {
                rig.fake.add_unit(&format!("{name}.scope"));
            }
            rig.fake.on_systemd(move |call| match call {
                Systemd::Stop { scope } if scope.to_string() == "togi-trial-a" => Some(if hang {
                    Reply::Hang
                } else {
                    Reply::Output {
                        success: false,
                        text: "Access denied".into(),
                    }
                }),
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
}

#[tokio::test(start_paused = true)]
async fn a_failing_verification_discovery_withholds_stopped_and_keeps_the_earlier_failure() {
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
}

#[tokio::test(start_paused = true)]
async fn cancelling_during_discovery_returns_cancelled_at_once_and_signals_nothing() {
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
}

#[tokio::test(start_paused = true)]
async fn cancelling_during_initial_process_scan_finishes_listed_scope_cleanup_and_keeps_errors() {
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
}

#[tokio::test(start_paused = true)]
async fn cancelling_during_initial_process_scan_without_a_known_scope_returns_at_once() {
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
}

#[tokio::test(start_paused = true)]
async fn cancelling_after_the_term_still_kills_and_stops_every_discovered_scope() {
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
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_sweep_whose_cleanup_fails_reports_cancelled_and_the_containment_failure() {
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
}

#[cfg(target_os = "linux")]
mod killed_owner {
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

    fn children_of(parent: u32) -> Vec<Escaped> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| parent_of(*pid) == Some(parent))
            .filter_map(Escaped::adopt)
            .collect()
    }

    fn frozen_or_gone(pid: u32) -> bool {
        if stat_of(pid).is_none_or(|stat| matches!(stat.state, 'Z' | 'X')) {
            return true;
        }
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
            return false;
        };
        let mut seen = false;
        for task in tasks.flatten() {
            let Ok(text) = std::fs::read_to_string(task.path().join("stat")) else {
                continue;
            };
            if !parse_stat(&text).is_ok_and(|stat| matches!(stat.state, 'T' | 't')) {
                return false;
            }
            seen = true;
        }
        seen
    }

    /// Owns the launched owner from launch until its observed exit and reap. Dropped while still
    /// holding it, it freezes the owner and waits until every thread is stopped, so no further
    /// fork can race the enumeration of its children, then kills the owner and every child it
    /// found, reaps the owner and waits for the children to die. It spawns nothing and signals
    /// no pid it did not pin or hold unreaped.
    struct OwnerGuard(Option<Leader>);

    impl OwnerGuard {
        fn owner(&self) -> &Leader {
            self.0
                .as_ref()
                .expect("the owner is held until it is reaped")
        }

        fn kill_and_reap(&mut self) -> ExitInfo {
            let owner = self.owner();
            assert!(owner.signal_group(GroupSignal::Kill).is_ok());
            assert!(poll_until(
                std::time::Instant::now() + Duration::from_secs(10),
                || owner.exit().is_ok_and(|exit| exit.is_some())
            ));
            self.0.take().expect("held above").reap().unwrap()
        }
    }

    impl Drop for OwnerGuard {
        fn drop(&mut self) {
            let Some(owner) = self.0.take() else {
                return;
            };
            let pid = owner.pid();
            let end = std::time::Instant::now() + Duration::from_secs(5);
            let _ = owner.signal_group(GroupSignal::Stop);
            let _ = poll_until(end, || frozen_or_gone(pid));
            let children = children_of(pid);
            let _ = owner.signal_group(GroupSignal::Kill);
            for child in &children {
                child.kill();
            }
            let end = std::time::Instant::now() + Duration::from_secs(5);
            if poll_until(end, || owner.exit().is_ok_and(|exit| exit.is_some())) {
                let _ = owner.reap();
            }
            let _ = poll_until(end, || children.iter().all(|child| !child.alive()));
        }
    }

    fn setup_owner(
        os: &Linux,
        dir: &std::path::Path,
        after_launch: impl FnOnce(&Leader),
    ) -> (Escaped, ScopeProcess) {
        let mut guard = OwnerGuard(Some(
            os.launch(&helper_command("scope-owner", dir)).unwrap(),
        ));
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
}
