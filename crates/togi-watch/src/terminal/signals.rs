//! Signals: SIGINT, SIGTERM and SIGHUP cancel the dashboard, SIGWINCH redraws it.
//!
//! The handlers stay installed for the rest of the process, as every Rust signal library leaves them.

use std::io;

use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use super::Event;

/// Starts listening and forwards the signals as events. Registration is done when this returns, so a signal sent
/// after it is never lost.
pub(super) fn listen(events: UnboundedSender<Event>) -> io::Result<JoinHandle<()>> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hang_up = signal(SignalKind::hangup())?;
    let mut resize = signal(SignalKind::window_change())?;
    Ok(tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                _ = interrupt.recv() => Event::Cancel,
                _ = terminate.recv() => Event::Cancel,
                _ = hang_up.recv() => Event::Cancel,
                _ = resize.recv() => Event::Resize,
            };
            if events.send(event).is_err() {
                return;
            }
        }
    }))
}
