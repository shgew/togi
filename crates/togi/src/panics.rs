//! What a panic does to the process (`docs/spec/runtime.md`, Exit codes): exit 2 with a `panic:` line.
//!
//! A panic on the thread that owns the run unwinds to [`main_with`], after the run's own cleanup ran on the way
//! (the session cancels the workload, joins its teardown and restores the offsets; the journal and the host lock
//! drop in order). A panic on any other thread never ends the process: a panic hook runs before the unwind, so
//! exiting there would also kill the panics a thread contains itself (the trial runtime's classifier and
//! supervision, the dashboard), mid-trial, before the owner restored anything. The hook only reports the panic
//! (held back while a dashboard owns the screen, then printed when it lets go) and the thread unwinds; whoever
//! waits on that thread sees it end (a closed channel, a failed join) and the owner's cleanup handles the rest.
//! The one thread whose loss nothing would notice, the signal listener, cancels every run when it dies.

use std::any::Any;
use std::cell::Cell;
use std::ffi::OsString;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::{self, ThreadId};

use togi_render::escape_text;

use crate::cli;
use crate::exit::Exit;
use crate::system::System;

/// The status a panic exits with.
pub const STATUS: Exit = Exit::Usage;

static OWNER: OnceLock<ThreadId> = OnceLock::new();
static OWNER_PANIC: Mutex<Option<String>> = Mutex::new(None);
static SCREEN_OWNED: AtomicBool = AtomicBool::new(false);
static HELD: Mutex<Vec<String>> = Mutex::new(Vec::new());

thread_local! {
    static CONTAINED: Cell<bool> = const { Cell::new(false) };
}

/// Runs `body` on this thread, containing a panic in it: the panic neither exits the process nor reaches the
/// caller as an unwind; its message is returned.
///
/// # Errors
///
/// The message of the panic.
pub fn contained<T>(body: impl FnOnce() -> T) -> Result<T, String> {
    let before = CONTAINED.replace(true);
    let outcome = panic::catch_unwind(AssertUnwindSafe(body));
    CONTAINED.set(before);
    outcome.map_err(|payload| message(&*payload).to_owned())
}

/// The message a panic payload carries.
#[must_use]
pub fn message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// Marks the calling thread as the one that owns the run and installs the hook.
pub fn install() {
    let _ = OWNER.set(thread::current().id());
    panic::set_hook(Box::new(hook));
}

/// Says whether a dashboard owns the screen. While it does, reports of panics on other threads are held back,
/// not printed over it; they are printed when it lets go.
pub fn screen_owned(owned: bool) {
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    SCREEN_OWNED.store(owned, Ordering::SeqCst);
    if !owned {
        for line in held.drain(..) {
            let _ = writeln!(io::stderr(), "{line}");
        }
    }
}

fn hook(info: &PanicHookInfo<'_>) {
    if CONTAINED.get() {
        return;
    }
    let text = describe(info);
    let current = thread::current();
    if OWNER.get() == Some(&current.id()) {
        *OWNER_PANIC.lock().unwrap_or_else(PoisonError::into_inner) = Some(text);
        return;
    }
    let line = format!(
        "togi: panic on thread {}: {}",
        current.name().unwrap_or("<unnamed>"),
        escape_text(&text)
    );
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    if SCREEN_OWNED.load(Ordering::SeqCst) {
        held.push(line);
    } else {
        let _ = writeln!(io::stderr(), "{line}");
    }
}

fn describe(info: &PanicHookInfo<'_>) -> String {
    let text = message(info.payload());
    match info.location() {
        Some(at) => format!("{text} ({at})"),
        None => text.to_owned(),
    }
}

/// The process's entry point: runs the command line `args` against `system` on the process's own streams, and
/// turns a panic on this thread into the `panic:` line and status 2.
pub fn run_process<I, T>(args: I, system: &dyn System) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    install();
    let run = panic::catch_unwind(AssertUnwindSafe(|| {
        cli::run_with(args, &mut io::stdout(), &mut io::stderr(), system)
    }));
    match run {
        Ok(exit) => exit,
        Err(payload) => {
            let text = OWNER_PANIC
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .unwrap_or_else(|| message(&*payload).to_owned());
            let _ = writeln!(io::stderr(), "panic: {}", escape_text(&text));
            STATUS
        }
    }
}

/// [`run_process`] over the process's own arguments, as `main`'s status.
pub fn main_with(system: &dyn System) -> ExitCode {
    run_process(std::env::args_os(), system).into()
}

#[cfg(test)]
mod tests;
