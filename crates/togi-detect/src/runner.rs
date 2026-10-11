use std::ffi::OsString;
use std::time::{Duration, Instant};

use togi_command::Output;

use crate::error::{Error, Operation};
use crate::journal::{ExitRules, classify};

pub(crate) const JOURNALCTL_BUDGET: Duration = Duration::from_secs(30);

#[cfg(target_os = "linux")]
const OUTPUT_LIMIT: usize = 64 << 20;

pub(crate) enum RunError {
    #[cfg_attr(
        all(not(target_os = "linux"), not(test)),
        expect(dead_code, reason = "only the Linux runner starts a command")
    )]
    Command(togi_command::Error),
    #[cfg_attr(
        target_os = "linux",
        expect(dead_code, reason = "only platforms without journalctl construct it")
    )]
    Unsupported,
}

pub(crate) trait Journalctl: Send {
    fn run(&self, args: &[OsString], remaining: Duration) -> Result<Output, RunError>;
}

pub(crate) struct Process {
    #[cfg(target_os = "linux")]
    program: OsString,
}

impl Process {
    pub(crate) fn system() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            program: OsString::from("journalctl"),
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn at(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

#[cfg(target_os = "linux")]
impl Journalctl for Process {
    fn run(&self, args: &[OsString], remaining: Duration) -> Result<Output, RunError> {
        let invocation = togi_command::Invocation::new(&self.program)
            .args(args)
            .output_limit(OUTPUT_LIMIT);
        #[expect(
            clippy::disallowed_methods,
            reason = "journalctl is detect's approved command"
        )]
        togi_command::run_blocking(&invocation, remaining).map_err(RunError::Command)
    }
}

#[cfg(not(target_os = "linux"))]
impl Journalctl for Process {
    fn run(&self, _args: &[OsString], _remaining: Duration) -> Result<Output, RunError> {
        Err(RunError::Unsupported)
    }
}

/// One seam call's journalctl commands, which share a single time budget.
pub(crate) struct Journal<'a> {
    runner: &'a dyn Journalctl,
    started: Instant,
    budget: Duration,
}

impl<'a> Journal<'a> {
    pub(crate) fn new(runner: &'a dyn Journalctl, budget: Duration) -> Self {
        Self {
            runner,
            started: Instant::now(),
            budget,
        }
    }

    pub(crate) fn query(
        &self,
        operation: &Operation,
        args: &[OsString],
        rules: ExitRules,
    ) -> Result<Vec<u8>, Error> {
        let remaining = self
            .budget
            .checked_sub(self.started.elapsed())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| Error::BudgetExhausted {
                operation: operation.clone(),
                budget: self.budget,
            })?;
        let output = self
            .runner
            .run(args, remaining)
            .map_err(|error| match error {
                RunError::Command(source) => Error::Command {
                    operation: operation.clone(),
                    source,
                },
                RunError::Unsupported => Error::Unsupported {
                    operation: operation.clone(),
                },
            })?;
        classify(operation, rules, output)
    }
}
