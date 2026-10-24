//! The `togi` command line: argument parsing, exit statuses, `--version` and `completion`.
//!
//! [`run()`] parses the arguments with clap, runs the chosen command against the given streams and
//! returns an [`Exit`]; `main` writes to the process's own streams and turns the result into the
//! process status.
//!
//! # Adding a command
//!
//! A command is one module and one variant:
//!
//! 1. Write `src/<name>.rs` with a `#[derive(clap::Args)] pub struct Args` for its flags and
//!    `pub fn run(args: Args, ctx: &mut Context<'_>) -> Exit`. Flatten [`args::ConfigArgs`] into
//!    `Args` if the command reads the configuration; `--state-dir` is already global and arrives
//!    as `ctx.state_dir`. Write output through `ctx.out` and `ctx.err`, and return the status
//!    from [`Exit`].
//! 2. Add `pub mod <name>;` here and one variant to [`cli::Command`] holding the module's `Args`,
//!    (the first paragraph of the struct's doc comment is the summary clap lists), and one arm to [`cli::Command::run`].
//!
//! Completions and help follow from the clap definition, so nothing else lists the command, and
//! the snapshot tests show the new text for review.
//!
//! # The host seam
//!
//! A command reaches the machine only through [`system::System`]: the platform, privileges, host
//! lock, default configuration file, boot id, clock, hardware checks, GRUB, the retry marker and
//! the reboot. [`run()`] uses the
//! real host; [`run_with`] takes any other, so a test on Linux can stand for another platform
//! (`check_platform` returning [`togi_hardware::Error::Unsupported`]), hold the host lock on a
//! temporary file or answer the hardware checks without touching hardware. `run`, `doctor` and
//! `emergency-stop` ask `check_platform` first, before they read the boot id, open the journal,
//! take the host lock or make a check, and report `hardware runs need Linux` with exit 1.

pub mod args;
pub mod cli;
pub mod completion;
pub mod dashboard;
pub mod day;
pub mod day_destination;
pub mod doctor;
pub mod emergency_stop;
pub mod events;
pub mod exit;
pub mod night;
pub mod night_end;
pub mod night_guard;
pub mod panics;
pub mod read;
pub mod report;
pub mod reset;
pub mod restart_limit;
pub mod run;
pub mod signals;
pub mod status;
pub mod system;
mod table;
pub mod watch;

pub use cli::{run, run_with};
pub use exit::Exit;
pub use panics::{main_with, run_process};
