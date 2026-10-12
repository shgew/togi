//! What a panic does to the process (`docs/spec/runtime.md`, Exit codes): exit 2 with a `panic:` line.
//!
//! A panic on the thread that owns the run unwinds to [`main_with`], after the run's own cleanup ran on the way
//! (the session cancels the workload, joins its teardown and restores the offsets; the journal and the host lock
//! drop in order). A panic on any other thread or tokio task exits at once, so no cleanup runs beside the owner's
//! own work. A thread that contains its own panics, as the dashboard's does, is left alone.

use std::any::Any;
use std::cell::Cell;
use std::ffi::OsString;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::process::ExitCode;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::{self, ThreadId};

use crate::cli;
use crate::exit::Exit;
use crate::system::System;

/// The status a panic exits with.
pub const STATUS: Exit = Exit::Usage;

static OWNER: OnceLock<ThreadId> = OnceLock::new();
static OWNER_PANIC: Mutex<Option<String>> = Mutex::new(None);

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

fn hook(info: &PanicHookInfo<'_>) {
    if CONTAINED.get() {
        return;
    }
    let text = describe(info);
    if OWNER.get() == Some(&thread::current().id()) {
        *OWNER_PANIC.lock().unwrap_or_else(PoisonError::into_inner) = Some(text);
        return;
    }
    let _ = writeln!(io::stderr(), "panic: {text}");
    std::process::exit(i32::from(STATUS.code()));
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
            let _ = writeln!(io::stderr(), "panic: {text}");
            STATUS
        }
    }
}

/// [`run_process`] over the process's own arguments, as `main`'s status.
pub fn main_with(system: &dyn System) -> ExitCode {
    run_process(std::env::args_os(), system).into()
}
