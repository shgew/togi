//! The process-lifetime listener for SIGINT, SIGTERM and SIGHUP that stops a run.
//!
//! The dashboard's own signal handling is tokio's, and tokio keeps every registration for the rest of the
//! process: once a dashboard has run, these signals no longer take their default action. `run` therefore
//! keeps its own listener from before the dashboard shows until the process exits, which cancels the run's
//! token however the dashboard came and went.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;

use togi_machine::CancellationToken;

static TOKENS: Mutex<Vec<(u64, CancellationToken)>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);
static LISTENER: OnceLock<Result<(), String>> = OnceLock::new();

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
/// # Errors
///
/// The runtime or the signal registration failed.
pub fn listen(token: &CancellationToken) -> io::Result<Listening> {
    LISTENER
        .get_or_init(|| start().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| io::Error::other(message.clone()))?;
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    tokens().push((id, token.clone()));
    Ok(Listening { id })
}

fn start() -> io::Result<()> {
    let (ready, started) = mpsc::channel();
    thread::Builder::new()
        .name("togi-signals".to_owned())
        .spawn(move || listener(&ready))?;
    started
        .recv()
        .map_err(|_| io::Error::other("the signal listener ended before it registered"))?
}

fn listener(ready: &mpsc::Sender<io::Result<()>>) {
    use tokio::signal::unix::{SignalKind, signal};
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(error));
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
                let _ = ready.send(Err(error));
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
            for (_, token) in tokens().iter() {
                token.cancel();
            }
        }
    });
}
