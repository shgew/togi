//! The leftover sweep and the preflight probe on the fake OS under paused time.

use std::time::Duration;

use tokio::time::Instant;

use super::harness::{local, rig, rig_edit};
use crate::error::{CleanupFailure, Error};
use crate::id::ScopeName;
use crate::os::fake::Reply;
use crate::os::{KillSignal, Systemd};
use crate::procfs::ScopeProcess;
use crate::sweep::{parse_listing, probe, sweep};
use crate::testing::TestBackend;
use crate::timing::Timing;

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
        assert_eq!(begun.elapsed(), secs(15));
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
async fn a_failing_verification_listing_or_discovery_is_containment() {
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
