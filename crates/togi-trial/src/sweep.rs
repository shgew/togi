use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use togi_machine::{CancellationToken, Stopped};
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, sleep_until, timeout_at};

use crate::error::{CleanupFailure, Containment, Error, SystemdError};
use crate::id::ScopeName;
use crate::identity::IdentityError;
use crate::os::{KillSignal, Os, Systemd};
use crate::supervisor::Env;

const POLL: Duration = Duration::from_millis(100);

/// Scope names from `systemctl list-units` output: the first token of each line, exact grammar
/// only, so a glob match or a look-alike unit is never touched.
pub(crate) fn parse_listing(output: &str) -> Vec<ScopeName> {
    output
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(ScopeName::from_unit)
        .collect()
}

fn says_missing(text: &str) -> bool {
    text.contains("not loaded") || text.contains("could not be found")
}

fn exited(output: &togi_command::Output, text: &str) -> SystemdError {
    SystemdError::Exit {
        status: output.status,
        output: text.trim().to_owned(),
    }
}

/// Runs one systemd call to `end`; `Ok(())` for success or a scope that is already gone.
pub(crate) async fn call<O: Os>(
    os: &O,
    operation: &Systemd,
    end: Instant,
) -> Result<(), SystemdError> {
    match timeout_at(end, os.systemd(operation, end)).await {
        Ok(Ok(output)) if output.status.success() => Ok(()),
        Ok(Ok(output)) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if says_missing(&text) {
                Ok(())
            } else {
                Err(exited(&output, &text))
            }
        }
        Ok(Err(error)) => Err(SystemdError::Command(error)),
        Err(elapsed) => Err(SystemdError::Timeout(elapsed)),
    }
}

async fn list<O: Os>(os: &O, end: Instant) -> Result<Vec<ScopeName>, SystemdError> {
    match timeout_at(end, os.systemd(&Systemd::ListScopes, end)).await {
        Ok(Ok(output)) if output.status.success() => {
            Ok(parse_listing(&String::from_utf8_lossy(&output.stdout)))
        }
        Ok(Ok(output)) => Err(exited(&output, &String::from_utf8_lossy(&output.stderr))),
        Ok(Err(error)) => Err(SystemdError::Command(error)),
        Err(elapsed) => Err(SystemdError::Timeout(elapsed)),
    }
}

fn observe(
    what: &'static str,
    source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> CleanupFailure {
    CleanupFailure::Observe {
        what,
        source: source.into(),
    }
}

fn contained(failures: Vec<CleanupFailure>) -> Error {
    let mut failures = failures.into_iter();
    let mut containment = Containment::new(failures.next().unwrap_or(CleanupFailure::Leftover));
    for failure in failures {
        containment.push(failure);
    }
    Error::Containment(containment.into())
}

/// Sends the same `operation` for every scope concurrently inside one slot.
async fn each_scope<O: Os>(
    os: &Rc<O>,
    scopes: &[ScopeName],
    end: Instant,
    signal: KillSignal,
    failures: &mut Vec<CleanupFailure>,
    stop: bool,
) {
    let mut calls = JoinSet::new();
    for scope in scopes {
        let os = Rc::clone(os);
        let scope = scope.clone();
        let kill = Systemd::Kill {
            scope: scope.clone(),
            signal,
        };
        calls.spawn_local(async move {
            let outcome = call(os.as_ref(), &kill, end).await;
            let stopped = match (&outcome, stop) {
                (Ok(()), true) => Some(
                    call(
                        os.as_ref(),
                        &Systemd::Stop {
                            scope: scope.clone(),
                        },
                        end,
                    )
                    .await,
                ),
                _ => None,
            };
            (scope, outcome, stopped)
        });
    }
    while let Some(Ok((scope, outcome, stopped))) = calls.join_next().await {
        if let Err(source) = outcome {
            failures.push(CleanupFailure::KillScope {
                scope: scope.to_string(),
                signal: signal.name(),
                source,
            });
        }
        if let Some(Err(source)) = stopped {
            failures.push(CleanupFailure::StopScope {
                scope: scope.to_string(),
                source,
            });
        }
    }
}

/// Resolves once `cancel` fires; `woken` is the notify its `on_cancel` callback signals.
async fn until_cancelled(cancel: &CancellationToken, woken: &Notify) {
    if !cancel.is_cancelled() {
        woken.notified().await;
    }
}

/// Clears leftovers of an earlier run under one absolute deadline: signals only through exact
/// scopes, verifies no unit, no scope process and no captured process remains. A failed
/// observation is recorded and the known scopes are still killed; every failure is kept.
///
/// Discovery stops `kill_slot` short of the deadline, so a hung listing or scan still leaves
/// time to SIGKILL and stop every scope already known. Cancellation before any scope is known
/// returns `Cancelled` at once. Once a scope is known, cancellation never drops the work: the
/// scan, signals, kill, stop and verification finish inside the same deadline, and the result
/// is `Cancelled` (joined with the containment failures, if cleanup did not confirm).
pub(crate) async fn sweep<O: Os>(
    env: &Env<O>,
    cancel: &CancellationToken,
) -> Result<(String, Stopped), Error> {
    let os = &env.os;
    let timing = env.timing;
    let woken = Arc::new(Notify::new());
    let _registration = {
        let woken = Arc::clone(&woken);
        cancel.on_cancel(move || woken.notify_one())
    };
    let begun = Instant::now();
    let deadline = begun + timing.teardown;
    let reserve = timing.kill_slot.min(timing.teardown / 2);
    let discovery_end = deadline.checked_sub(reserve).unwrap_or(begun);
    let mut failures = Vec::new();
    let listed = tokio::select! {
        () = until_cancelled(cancel, &woken) => return Err(Error::Cancelled),
        listed = list(os.as_ref(), discovery_end) => listed,
    };
    let units =
        listed.map_err(|source| contained(vec![observe("the trial scope list", source)]))?;
    let scanned = tokio::select! {
        () = until_cancelled(cancel, &woken), if units.is_empty() => return Err(Error::Cancelled),
        scanned = os.scope_processes(discovery_end) => scanned,
    };
    let captured = match scanned {
        Ok(captured) => captured,
        Err(error) => {
            failures.push(observe("the trial processes", error));
            Vec::new()
        }
    };
    let scopes: Vec<ScopeName> = units
        .into_iter()
        .chain(captured.iter().map(|process| process.scope.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    // The grace never eats the reserved kill slot; a discovery that ran to its end leaves none.
    let grace_end = discovery_end.min(Instant::now() + timing.stop_grace);
    if Instant::now() < grace_end {
        for signal in [KillSignal::Cont, KillSignal::Term] {
            each_scope(os, &scopes, grace_end, signal, &mut failures, false).await;
        }
    }
    while Instant::now() < grace_end {
        match os.survivors(&captured, grace_end).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                failures.push(observe("a captured trial process", error));
                break;
            }
        }
        tokio::select! {
            () = sleep(POLL) => {}
            () = sleep_until(grace_end) => {}
        }
    }

    let kill_end = deadline.min(Instant::now() + timing.kill_slot);
    each_scope(os, &scopes, kill_end, KillSignal::Kill, &mut failures, true).await;

    let verify_end = deadline.min(Instant::now() + timing.drain);
    let mut confirmed = false;
    let mut observed = true;
    while observed && Instant::now() < verify_end {
        let survivors = os.survivors(&captured, verify_end).await;
        let remaining = list(os.as_ref(), verify_end).await;
        let fresh = os.scope_processes(verify_end).await;
        match (survivors, remaining, fresh) {
            (Ok(0), Ok(remaining), Ok(fresh)) if remaining.is_empty() && fresh.is_empty() => {
                confirmed = Instant::now() <= deadline;
                break;
            }
            (survivors, remaining, fresh) => {
                if let Err(error) = survivors {
                    failures.push(observe("a captured trial process", error));
                    observed = false;
                }
                if let Err(source) = remaining {
                    failures.push(observe("the trial scope list", source));
                    observed = false;
                }
                if let Err(error) = fresh {
                    failures.push(observe("the trial processes", error));
                    observed = false;
                }
            }
        }
        if observed {
            tokio::select! {
                () = sleep(POLL) => {}
                () = sleep_until(verify_end) => {}
            }
        }
    }
    if !confirmed && observed {
        failures.push(CleanupFailure::Leftover);
    }
    let cancelled = cancel.is_cancelled();
    if !failures.is_empty() || !confirmed {
        let failure = contained(failures);
        return Err(if cancelled {
            Error::join_after(Error::Cancelled, vec![failure])
        } else {
            failure
        });
    }
    if cancelled {
        return Err(Error::Cancelled);
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the sweep verified no trial unit, scope process or captured process remains"
    )]
    let stopped = Stopped::confirmed();
    Ok((
        format!(
            "removed {} leftover trial scopes; no trial unit or process remains",
            scopes.len()
        ),
        stopped,
    ))
}

/// The preflight check: a throwaway scope pinned to CPU 0 is created and ends, within 30 s.
pub(crate) async fn probe<O: Os>(env: &Env<O>) -> Result<String, Error> {
    let identity = env.identity.ok_or(IdentityError::NotConfigured)?;
    let unit = format!("togi-preflight-{}", std::process::id());
    let end = Instant::now() + env.timing.probe;
    let operation = Systemd::Probe { unit, identity };
    match timeout_at(end, env.os.systemd(&operation, end)).await {
        Ok(Ok(output)) if output.status.success() => {
            Ok("scope confined to cpu 0 created".to_owned())
        }
        Ok(Ok(output)) => Err(Error::Probe(exited(
            &output,
            &String::from_utf8_lossy(&output.stderr),
        ))),
        Ok(Err(error)) => Err(Error::Probe(SystemdError::Command(error))),
        Err(elapsed) => Err(Error::Probe(SystemdError::Timeout(elapsed))),
    }
}
