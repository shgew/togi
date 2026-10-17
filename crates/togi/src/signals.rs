//! The process-lifetime listener for SIGINT, SIGTERM and SIGHUP that stops a run.
//!
//! The dashboard's own signal handling is tokio's, and tokio keeps every registration for the rest of the
//! process: once a dashboard has run, these signals no longer take their default action. `run` therefore
//! keeps its own listener from before the dashboard shows until the process exits, which cancels the run's
//! token however the dashboard came and went. If the listener itself dies by a panic it cancels every token, so
//! a run stops and cleans up instead of going on deaf to its signals.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;

use togi_machine::CancellationToken;

static TOKENS: Mutex<Vec<(u64, CancellationToken)>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);
static LISTENER: OnceLock<Result<(), Arc<StartError>>> = OnceLock::new();
static LOST: AtomicBool = AtomicBool::new(false);

/// Why a run cannot listen for the signals that stop it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The process's listener did not start; every later run in this process gets the same failure.
    #[error("start the signal listener")]
    Start(#[source] Arc<StartError>),
    /// The listener of an earlier run in this process died by a panic.
    #[error("the signal listener stopped")]
    Lost,
}

/// Why the process's listener did not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("spawn the listener thread")]
    Spawn(#[source] io::Error),
    #[error("build the listener runtime")]
    Runtime(#[source] io::Error),
    #[error("register the signal handlers")]
    Register(#[source] io::Error),
    #[error("the listener ended before it registered")]
    Ended,
}

fn tokens() -> MutexGuard<'static, Vec<(u64, CancellationToken)>> {
    TOKENS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Cancels its token on SIGINT, SIGTERM or SIGHUP while it lives. The listener thread itself outlives it.
#[derive(Debug)]
pub struct Listening {
    id: u64,
}

impl Drop for Listening {
    fn drop(&mut self) {
        tokens().retain(|(id, _)| *id != self.id);
    }
}

/// Starts the process's listener if it is not running and has it cancel `token`.
///
/// The token is registered before the listener starts, so a signal that arrives as the listener comes up
/// still cancels it; it is deregistered again if the listener cannot start.
///
/// # Errors
///
/// The runtime or the signal registration failed, or the listener of an earlier run in this process died.
pub fn listen(token: &CancellationToken) -> Result<Listening, Error> {
    listen_after(token, start)
}

fn listen_after(
    token: &CancellationToken,
    start: impl FnOnce() -> Result<(), StartError>,
) -> Result<Listening, Error> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    tokens().push((id, token.clone()));
    let listening = Listening { id };
    LISTENER
        .get_or_init(|| start().map_err(Arc::new))
        .as_ref()
        .map_err(|error| Error::Start(Arc::clone(error)))?;
    if LOST.load(Ordering::SeqCst) {
        return Err(Error::Lost);
    }
    Ok(listening)
}

/// Cancels every registered token.
fn cancel_all() {
    for (_, token) in tokens().iter() {
        token.cancel();
    }
}

/// Runs the listener and, if it dies by a panic, cancels every registered token and refuses later
/// registrations: a run must stop and clean up rather than go on deaf to its signals.
fn guarded(body: impl FnOnce()) {
    struct Lost;
    impl Drop for Lost {
        fn drop(&mut self) {
            if thread::panicking() {
                LOST.store(true, Ordering::SeqCst);
                cancel_all();
            }
        }
    }
    let _lost = Lost;
    body();
}

fn start() -> Result<(), StartError> {
    let (ready, started) = mpsc::channel();
    thread::Builder::new()
        .name("togi-signals".to_owned())
        .spawn(move || guarded(|| listener(&ready)))
        .map_err(StartError::Spawn)?;
    started.recv().map_err(|_| StartError::Ended)?
}

fn listener(ready: &mpsc::Sender<Result<(), StartError>>) {
    use tokio::signal::unix::{SignalKind, signal};
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(StartError::Runtime(error)));
            return;
        }
    };
    runtime.block_on(async {
        let registered = (|| {
            Ok::<_, io::Error>((
                signal(SignalKind::interrupt())?,
                signal(SignalKind::terminate())?,
                signal(SignalKind::hangup())?,
            ))
        })();
        let (mut interrupt, mut terminate, mut hangup) = match registered {
            Ok(signals) => signals,
            Err(error) => {
                let _ = ready.send(Err(StartError::Register(error)));
                return;
            }
        };
        let _ = ready.send(Ok(()));
        loop {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
                _ = hangup.recv() => {}
            }
            cancel_all();
        }
    });
}

#[cfg(test)]
mod tests;
