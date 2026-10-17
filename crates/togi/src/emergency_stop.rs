//! `togi emergency-stop`: stop what a dead togi left running and set every core to 0, and nothing else.

use std::fmt;
use std::io::Write;
use std::sync::LazyLock;

use clap::Args as ClapArgs;
use togi_config::Config;
use togi_hostlock::Error as LockError;
use togi_machine::{
    CancellationToken, CoreInfo, CoreSet, Error as MachineError, Machine, ObservedOffset,
};
use togi_session::{Readback, Zeroed, zero_and_read_back};

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::report::{exit_with, fail};

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[(
        "sudo togi emergency-stop",
        "Stop leftover trials, then set every core to 0 and read each back",
    )])
});

/// Internal emergency recovery that stops leftover trials and sets every core to offset 0
///
/// Internal recovery for a togi process that died with trial workloads or Curve Optimizer offsets
/// left behind; the tuning service runs it after an abnormal exit. It refuses outside Linux and
/// without root, takes the host lock, validates the CPU, the SMU driver and the core
/// topology, stops every stale togi trial scope and confirms none is left. Only then does it write
/// 0 to every core and read every core back.
///
/// It only ever writes 0: it takes no offsets and no configuration, and neither reads nor appends
/// to the journal. Offsets stay as they were when another togi process holds the host lock, when
/// validation fails, when a stale trial scope cannot be stopped or when the core slots cannot be
/// mapped. Exit status 0 means every core read back 0; 3, that the host lock is busy; 1, anything
/// else, with the cause on stderr.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {}

const COMMAND: &str = "emergency-stop";

/// What a completed stop did.
#[derive(Debug, Eq, PartialEq)]
pub struct Recovered {
    /// What the sweep of the stale trial scopes found.
    pub swept: String,
    /// How many cores were written 0 and read back 0.
    pub cores: usize,
}

/// A step before the first write that could not be confirmed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
    Topology,
    Validation,
    Sweep,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Topology => "read the core topology",
            Self::Validation => "validate the SMU interface",
            Self::Sweep => "stop the stale trial scopes",
        })
    }
}

/// Why a stop did not end with every core read back 0.
#[derive(Debug)]
pub enum Failure {
    /// `step` failed, so no offset was written.
    Withheld { step: Step, error: MachineError },
    /// The write of 0 or a readback did not confirm every one of `cores` at 0.
    NotZero { zeroed: Zeroed, cores: usize },
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Withheld { step, error } => {
                write!(
                    f,
                    "{step}: {}; offsets unchanged",
                    togi_render::error_chain(error)
                )
            }
            Self::NotZero { zeroed, cores } => {
                let confirmed = zeroed
                    .readbacks
                    .iter()
                    .filter(|readback| at_zero(readback))
                    .count();
                let missing = cores - confirmed;
                match &zeroed.zero {
                    Ok(()) => write!(f, "{missing} of {cores} cores did not read back 0"),
                    Err(error) => write!(
                        f,
                        "setting every core to 0 failed: {}; {missing} of {cores} cores did not read back 0",
                        togi_render::error_chain(error)
                    ),
                }
            }
        }
    }
}

/// Stops what a dead togi left running, then sets every core to 0 and reads each back.
///
/// The order is the gate of the session's emergency: the topology and the validated SMU interface
/// come first, the sweep of the stale trial scopes proves no workload runs, and only the proofs of
/// both let the zero-only write through. Every step before the write that fails leaves the SMU
/// as it was. Progress and every core's readback go to `report`; a failed report write changes
/// nothing, since the zeroing matters more than its log.
///
/// # Errors
///
/// [`Failure::Withheld`] before any write; [`Failure::NotZero`] once the write was made but a
/// core did not read back 0 or could not be read.
pub fn stop(machine: &mut Machine, report: &mut dyn Write) -> Result<Recovered, Failure> {
    let topology = machine.host.topology().map_err(withheld(Step::Topology))?;
    let total = topology.len();
    let cores = core_set(&topology).map_err(withheld(Step::Topology))?;
    let validated = machine
        .host
        .validate_smu()
        .map_err(withheld(Step::Validation))?;
    let swept = machine
        .trials
        .sweep(&CancellationToken::new())
        .map_err(withheld(Step::Sweep))?;
    say(report, format_args!("trial scopes: {}", swept.detail));
    let zeroed = zero_and_read_back(&mut *machine.smu, validated, &swept.stopped, cores);
    for readback in &zeroed.readbacks {
        say(report, readback_line(readback));
    }
    if zeroed.is_clean() {
        Ok(Recovered {
            swept: swept.detail,
            cores: zeroed.readbacks.len(),
        })
    } else {
        Err(Failure::NotZero {
            zeroed,
            cores: total,
        })
    }
}

fn withheld(step: Step) -> impl FnOnce(MachineError) -> Failure {
    move |error| Failure::Withheld { step, error }
}

/// The cores of the topology, or why it cannot be trusted: a machine without cores has nothing to
/// confirm at 0.
fn core_set(topology: &[CoreInfo]) -> Result<CoreSet, MachineError> {
    if topology.is_empty() {
        return Err(MachineError::Other(
            "the topology lists no core to zero".into(),
        ));
    }
    Ok(topology.iter().map(|info| info.core).collect())
}

fn at_zero(readback: &Readback) -> bool {
    matches!(readback.result, Ok(ObservedOffset(0)))
}

fn readback_line(readback: &Readback) -> String {
    match &readback.result {
        Ok(ObservedOffset(0)) => format!("core {:02} read back 0", readback.core),
        Ok(offset) => format!("core {:02} reads {}", readback.core, offset.0),
        Err(error) => format!(
            "core {:02} unreadable: {}",
            readback.core,
            togi_render::error_chain(error)
        ),
    }
}

fn say(report: &mut dyn Write, line: impl fmt::Display) {
    let _ = writeln!(report, "togi {COMMAND}: {line}");
}

/// Stops the stale trials and zeroes every core under the host lock; see [`stop`].
pub fn run(_args: &Args, ctx: &mut Context<'_>) -> Exit {
    let system = ctx.system;
    if let Err(error) = system.check_platform() {
        return fail(ctx, COMMAND, error);
    }
    if !system.privileged() {
        return fail(
            ctx,
            COMMAND,
            "needs root to write the SMU; offsets unchanged",
        );
    }
    let lock = match system.acquire_host_lock() {
        Ok(lock) => lock,
        Err(error @ LockError::Busy { .. }) => {
            return exit_with(
                ctx,
                COMMAND,
                format!("{error}; offsets unchanged"),
                Exit::Locked,
            );
        }
        Err(error) => {
            return fail(ctx, COMMAND, format!("{error}; offsets unchanged"));
        }
    };
    let mut machine = match system.open_machine(&Config::default(), ctx.state_dir, &lock) {
        Ok(machine) => machine,
        Err(error) => {
            return fail(
                ctx,
                COMMAND,
                format!("{}; offsets unchanged", togi_render::error_chain(&error)),
            );
        }
    };
    match stop(&mut machine, &mut *ctx.err) {
        Ok(recovered) => {
            say(
                &mut *ctx.err,
                format_args!("every core read back 0 ({} cores)", recovered.cores),
            );
            Exit::Ok
        }
        Err(failure) => fail(ctx, COMMAND, failure),
    }
}
