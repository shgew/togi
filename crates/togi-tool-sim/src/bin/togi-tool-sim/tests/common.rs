use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use jiff::tz::TimeZone;
use togi_journal::{Event, MemFs};
use togi_machine::{CancellationToken, ControllerMode, ResetKind, TargetOffset};
use togi_render::Renderer;
use togi_sim::{ScriptedMachine, ScriptedTopology, TrialOutcome};
use togi_simrun::{Persistence, SimInput, SimOutcome};

use crate::{Command, SessionArgs, log::Log, session};

pub(crate) struct Ran {
    pub code: Option<i32>,
    pub stderr: String,
    pub stdout: String,
}

#[derive(Clone, Default)]
struct Shared(Rc<RefCell<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct Host {
    pub states: BTreeMap<PathBuf, MemFs>,
}

/// Preserve the watchdog cuts after each workload prefix needed by the cap and resume tests.
pub(super) const WATCHDOG_MACHINE: ScriptedMachine = ScriptedMachine::new(
    ScriptedTopology::FourCores,
    TargetOffset::ZERO,
    |trial, _| {
        let ControllerMode::Managed { rotation, .. } = trial.controller;
        let nanos = match rotation {
            65 => 4_695_930,
            133 => 205_424,
            134 => 433_244_441,
            202 => 1_424_415_247,
            _ => return TrialOutcome::Pass,
        };
        TrialOutcome::Reset {
            stalled: None,
            kind: ResetKind::Watchdog,
            at: Duration::from_nanos(nanos),
        }
    },
);

impl Host {
    pub(super) fn run<S: AsRef<OsStr>>(&mut self, args: impl IntoIterator<Item = S>) -> Ran {
        self.execute(args, togi_simrun::simulate)
    }

    pub(super) fn run_scripted<S: AsRef<OsStr>>(
        &mut self,
        args: impl IntoIterator<Item = S>,
        script: ScriptedMachine,
    ) -> Ran {
        self.execute_scripted(args, script, togi_simrun::simulate)
    }

    pub(super) fn execute_scripted<S: AsRef<OsStr>>(
        &mut self,
        args: impl IntoIterator<Item = S>,
        script: ScriptedMachine,
        execute: impl FnOnce(SimInput) -> Result<SimOutcome, togi_simrun::SimError>,
    ) -> Ran {
        self.execute(args, |mut input| {
            // Only the trial outcomes change: the CLI's prepared machine state must be what this
            // script resumes from, so a preparation regression cannot be repaired here.
            let simulator = togi_simrun::resume_simulator(&input.persistence, script, 1).unwrap();
            assert_eq!(simulator.persisted(), input.simulator.persisted());
            input.simulator = simulator;
            execute(input)
        })
    }

    pub(super) fn execute<S: AsRef<OsStr>>(
        &mut self,
        args: impl IntoIterator<Item = S>,
        execute: impl FnOnce(SimInput) -> Result<SimOutcome, togi_simrun::SimError>,
    ) -> Ran {
        let output = Shared::default();
        let mut log = Log::buffered(output.clone());
        let args = std::iter::once(OsString::from("togi-tool-sim"))
            .chain(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        let code = match crate::parse(args, &mut log) {
            Err(code) => code,
            Ok(cli) => match cli.command {
                None => session::run_with(
                    cli.session,
                    &mut log,
                    |args: SessionArgs, log| {
                        let path = args
                            .state_dir
                            .filter(|path| !path.as_os_str().is_empty())
                            .unwrap_or_else(|| {
                                let path = PathBuf::from("/temporary/togi-sim-memory");
                                log.announce(&format!("sim: state directory {}", path.display()));
                                path
                            });
                        Ok(Persistence::Memory(
                            self.states.entry(path).or_default().clone(),
                        ))
                    },
                    || {
                        Ok(session::Context {
                            cancel: CancellationToken::new(),
                            renderer: Renderer::default(),
                            zone: TimeZone::UTC,
                        })
                    },
                    execute,
                ),
                Some(Command::ScriptedContract | Command::Scripted(_)) => {
                    panic!("the named scripts' CLI protocol is exercised by the process tests")
                }
            },
        };
        log.finish().unwrap();
        let stderr = String::from_utf8(output.0.borrow().clone()).unwrap();
        Ran {
            code: Some(i32::from(code)),
            stderr,
            stdout: String::new(),
        }
    }

    pub(super) fn events(&self, path: &str) -> Vec<Event> {
        togi_simrun::events(&Persistence::Memory(self.states[Path::new(path)].clone())).unwrap()
    }
}

pub(crate) fn sim<S: AsRef<OsStr>>(args: impl IntoIterator<Item = S>) -> Ran {
    Host::default().run(args)
}
