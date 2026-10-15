use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use jiff::tz::TimeZone;
use togi_journal::{Event, MemFs};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_sim::file::Source;
use togi_simrun::{Persistence, SimInput, SimOutcome};
use togi_tool_sim::{Suite, SuiteError};
use togi_tool_trialfacts::Extracts;

use crate::{Command, SessionArgs, log::Log, machine, session, suite};

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
    pub files: BTreeMap<PathBuf, String>,
    pub states: BTreeMap<PathBuf, MemFs>,
}

struct Texts(BTreeMap<PathBuf, String>);

impl Source for Texts {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.0
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        if self.0.contains_key(path) || self.0.keys().any(|entry| entry.starts_with(path)) {
            Ok(path.to_path_buf())
        } else {
            Err(io::ErrorKind::NotFound.into())
        }
    }
}

impl Host {
    pub(super) fn put(&mut self, path: impl Into<PathBuf>, text: impl Into<String>) {
        self.files.insert(path.into(), text.into());
    }

    pub(super) fn run<S: AsRef<OsStr>>(&mut self, args: impl IntoIterator<Item = S>) -> Ran {
        self.execute(args, togi_simrun::simulate)
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
        let source = Texts(self.files.clone());
        let load = |path: Option<&Path>, seed, replay, extracts: &mut Extracts| {
            machine::load_with(path, seed, replay, extracts, |path| {
                togi_sim::file::load_from(path, &source).map(|(definition, _)| definition)
            })
        };
        let load_suite = |path: &Path| {
            let text = source
                .read_to_string(path)
                .map_err(|source| SuiteError::Read {
                    path: path.to_owned(),
                    source,
                })?;
            Suite::parse(&text, path.parent().unwrap_or_else(|| Path::new("")))
        };
        let code = match crate::parse(args, &mut log) {
            Err(code) => code,
            Ok(cli) => match cli.command {
                None => session::run_with(
                    cli.session,
                    &mut log,
                    |args| {
                        load(
                            args.machine.as_deref(),
                            args.seed,
                            args.replay_facts,
                            &mut Extracts::new(),
                        )
                    },
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
                Some(Command::Suite(mut args)) => {
                    args.jobs.get_or_insert(std::num::NonZeroUsize::MIN);
                    let result = load_suite(&args.suite)
                        .map_err(suite::Error::from)
                        .and_then(|loaded| {
                            suite::run_shard_with(
                                &args,
                                &mut log,
                                &loaded,
                                &CancellationToken::new(),
                                |run, extracts| {
                                    load(run.machine.as_deref(), run.seed, run.replay, extracts)
                                },
                                |path, text| {
                                    self.files.insert(path.to_owned(), text);
                                    Ok(())
                                },
                            )
                        });
                    match result {
                        Ok(true) => 0,
                        Ok(false) => 1,
                        Err(error) => {
                            let _ = writeln!(log, "sim: {error}");
                            1
                        }
                    }
                }
                Some(Command::Check(args)) => {
                    let result = load_suite(&args.suite)
                        .map_err(suite::Error::from)
                        .and_then(|loaded| {
                            let entries: Vec<_> = self
                                .files
                                .keys()
                                .filter(|path| path.parent() == Some(args.manifests.as_path()))
                                .cloned()
                                .collect();
                            suite::check_with(
                                &args,
                                &loaded,
                                || Ok(entries.into_iter().map(Ok)),
                                |path| source.read_to_string(path),
                            )
                        });
                    match result {
                        Ok(()) => 0,
                        Err(error) => {
                            let _ = writeln!(log, "sim: {error}");
                            1
                        }
                    }
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
