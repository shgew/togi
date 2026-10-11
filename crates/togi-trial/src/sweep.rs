use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Duration;

use togi_machine::Stopped;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, sleep_until, timeout_at};

use crate::error::{CleanupFailure, Containment, Error};
use crate::id::ScopeName;
use crate::identity::IdentityError;
use crate::os::{KillSignal, Os, Systemd};
use crate::procfs::ScopeProcess;
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

type Boxed = Box<dyn std::error::Error + Send + Sync>;

/// Runs one systemd call to `end`; `Ok(())` for success or a scope that is already gone.
async fn call<O: Os>(os: &O, operation: &Systemd, end: Instant) -> Result<(), Boxed> {
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
                Err(format!("exited with {}: {}", output.status, text.trim()).into())
            }
        }
        Ok(Err(error)) => Err(Box::new(error)),
        Err(elapsed) => Err(Box::new(elapsed)),
    }
}

async fn list<O: Os>(os: &O, end: Instant) -> Result<Vec<ScopeName>, Boxed> {
    match timeout_at(end, os.systemd(&Systemd::ListScopes, end)).await {
        Ok(Ok(output)) if output.status.success() => {
            Ok(parse_listing(&String::from_utf8_lossy(&output.stdout)))
        }
        Ok(Ok(output)) => Err(format!(
            "list-units exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into()),
        Ok(Err(error)) => Err(Box::new(error)),
        Err(elapsed) => Err(Box::new(elapsed)),
    }
}

fn observe(what: &'static str, source: Boxed) -> Error {
    Error::Containment(Containment::new(CleanupFailure::Observe { what, source }).into())
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

fn gone<O: Os>(os: &O, captured: &[ScopeProcess]) -> Result<bool, Boxed> {
    for process in captured {
        if os
            .process_alive(process)
            .map_err(|error| Box::new(error) as Boxed)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Clears leftovers of an earlier run under one absolute deadline: signals only through exact
/// scopes, verifies no unit, no scope process and no captured process remains.
pub(crate) async fn sweep<O: Os>(env: &Env<O>) -> Result<(String, Stopped), Error> {
    let os = &env.os;
    let timing = env.timing;
    let deadline = Instant::now() + timing.teardown;
    let units = list(os.as_ref(), deadline)
        .await
        .map_err(|source| observe("the trial scope list", source))?;
    let captured = os
        .scope_processes()
        .map_err(|error| observe("the trial processes", Box::new(error)))?;
    let scopes: Vec<ScopeName> = units
        .into_iter()
        .chain(captured.iter().map(|process| process.scope.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut failures = Vec::new();

    let grace_end = deadline.min(Instant::now() + timing.stop_grace);
    for signal in [KillSignal::Cont, KillSignal::Term] {
        each_scope(os, &scopes, grace_end, signal, &mut failures, false).await;
    }
    while Instant::now() < grace_end {
        if gone(os.as_ref(), &captured)
            .map_err(|source| observe("a captured trial process", source))?
        {
            break;
        }
        sleep(POLL.min(grace_end.saturating_duration_since(Instant::now()))).await;
    }

    let kill_end = deadline.min(Instant::now() + timing.kill_slot);
    each_scope(os, &scopes, kill_end, KillSignal::Kill, &mut failures, true).await;

    let verify_end = deadline.min(Instant::now() + timing.drain);
    let mut leftover = true;
    while Instant::now() < verify_end {
        let processes_gone = gone(os.as_ref(), &captured)
            .map_err(|source| observe("a captured trial process", source))?;
        let remaining = list(os.as_ref(), verify_end)
            .await
            .map_err(|source| observe("the trial scope list", source))?;
        let fresh = os
            .scope_processes()
            .map_err(|error| observe("the trial processes", Box::new(error)))?;
        if processes_gone && remaining.is_empty() && fresh.is_empty() {
            leftover = false;
            break;
        }
        tokio::select! {
            () = sleep(POLL) => {}
            () = sleep_until(verify_end) => {}
        }
    }
    if leftover {
        failures.push(CleanupFailure::Leftover);
    }
    let mut failures = failures.into_iter();
    match failures.next() {
        None => {
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
        Some(first) => {
            let mut containment = Containment::new(first);
            for failure in failures {
                containment.push(failure);
            }
            Err(Error::Containment(containment.into()))
        }
    }
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
        Ok(Ok(output)) => Err(Error::Probe {
            detail: format!(
                "systemd-run exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            source: None,
        }),
        Ok(Err(error)) => Err(Error::Probe {
            detail: "systemd-run".to_owned(),
            source: Some(Box::new(error)),
        }),
        Err(elapsed) => Err(Error::Probe {
            detail: "systemd-run timed out".to_owned(),
            source: Some(Box::new(elapsed)),
        }),
    }
}
