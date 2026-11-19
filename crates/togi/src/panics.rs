//! What a panic does to the process (`docs/spec/runtime.md`, Exit codes): exit 2 with a `panic:` line.
//!
//! A panic on the thread that owns the run unwinds to [`main_with`], after the run's own cleanup ran on the way
//! (the session cancels the workload, joins its teardown and restores the offsets; the journal and the host lock
//! drop in order). A panic on any other thread never ends the process: a panic hook runs before the unwind, so
//! exiting there would kill the process mid-trial, before the owner restored anything. The hook only reports
//! the panic and the thread unwinds; whoever waits on that thread sees it end (a closed channel, a failed
//! join) and the owner's cleanup handles the rest.
//! The one thread whose loss nothing would notice, the signal listener, cancels every run when it dies.

use std::any::Any;
use std::ffi::OsString;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::process::ExitCode;
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
    let text = describe(info);
    let current = thread::current();
    if OWNER.get() == Some(&current.id()) {
        *OWNER_PANIC.lock().unwrap_or_else(PoisonError::into_inner) = Some(text);
        return;
    }
    let _ = writeln!(
        io::stderr(),
        "togi: panic on thread {}: {}",
        current.name().unwrap_or("<unnamed>"),
        escape_text(&text)
    );
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
