use togi_backend::Line;
use togi_machine::{Backend, CoreId, CpuId, Sample, Signal, TrialFailure};

const SYSTEMD_STARTUP_FAILURES: [&str; 2] =
    ["Failed to start transient scope unit", "Failed to execute"];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
    Watched,
}

impl Stream {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout.log",
            Self::Stderr => "stderr.log",
            Self::Watched => "watched output",
        }
    }
}

/// What the supervisor saw, in the order it saw it; instances are indices into the trial plan.
#[derive(Debug)]
pub(crate) enum Observation {
    Line {
        instance: usize,
        pid: u32,
        stream: Stream,
        line: Line,
        text: String,
    },
    /// A launcher exited before teardown began.
    UnexpectedExit {
        instance: usize,
        detail: String,
    },
    /// A thread ran on a CPU outside the instance's allowed set.
    Escape {
        cpu: CpuId,
        pid: u32,
        tid: u32,
    },
    Stall {
        instance: usize,
        pid: u32,
        cpu: u32,
        detail: String,
    },
    LineTooLong {
        instance: usize,
        stream: Stream,
    },
    /// An output I/O failure: surfaces always, whatever else was selected.
    OutputError {
        instance: usize,
        stream: Stream,
        error: String,
    },
    SamplingLoss {
        instance: usize,
        what: String,
    },
    NeverEntered {
        instance: usize,
        scope: String,
    },
}

#[derive(Debug, PartialEq)]
pub(crate) enum Report {
    Progress(String),
    Warning(Sample),
    Signal {
        core: CoreId,
        signal: Signal,
        detail: String,
    },
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Decision {
    pub(crate) stop: bool,
    pub(crate) reports: Vec<Report>,
}

/// What the evidence concluded; the trial's result is built from it.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Verdict {
    pub(crate) failure: Option<TrialFailure>,
    pub(crate) inconclusive: Option<String>,
    pub(crate) escaped: Vec<CpuId>,
    /// Output I/O errors, kept whatever else was selected.
    pub(crate) diagnostics: Vec<String>,
}

#[derive(Debug)]
struct PendingExit {
    instance: usize,
    detail: String,
}

/// The one selector, for live and teardown observations alike. Precedence: escape, then
/// computation error, stall or unexpected exit (first selected core), then setup error, then
/// sampling or observation loss. Oversized-line errors are suppressed only by stronger evidence.
#[derive(Debug)]
pub(crate) struct Selector {
    backend: Backend,
    cores: Vec<CoreId>,
    first_cpus: Vec<CpuId>,
    failure: Option<TrialFailure>,
    escaped: Vec<CpuId>,
    inconclusive: Option<String>,
    setup_error: bool,
    line_too_long: bool,
    pending_exit: Option<PendingExit>,
    diagnostics: Vec<String>,
}

impl Selector {
    /// `instances` pairs each instance's core with the first CPU it was told to use, which names
    /// the CPU of an affinity error whose own number cannot be read.
    pub(crate) fn new(backend: Backend, instances: &[(CoreId, CpuId)]) -> Self {
        Self {
            backend,
            cores: instances.iter().map(|(core, _)| *core).collect(),
            first_cpus: instances.iter().map(|(_, cpu)| *cpu).collect(),
            failure: None,
            escaped: Vec::new(),
            inconclusive: None,
            setup_error: false,
            line_too_long: false,
            pending_exit: None,
            diagnostics: Vec::new(),
        }
    }

    fn core(&self, instance: usize) -> CoreId {
        self.cores[instance]
    }

    fn escaped(&self) -> bool {
        !self.escaped.is_empty()
    }

    /// Sets the reason only when nothing stronger or earlier explains the trial.
    fn weak_reason(&mut self, reason: impl FnOnce() -> String) {
        if self.inconclusive.is_none() && self.failure.is_none() && !self.escaped() {
            self.inconclusive = Some(reason());
        }
    }

    fn escape(&mut self, cpu: CpuId, warning: &str, pid: u32, tid: u32) -> Decision {
        if !self.escaped() {
            self.escaped.push(cpu);
            self.failure = None;
            self.inconclusive = None;
        }
        Decision {
            stop: true,
            reports: vec![Report::Warning(Sample {
                warning: warning.to_owned(),
                pid,
                tid,
                cpu,
            })],
        }
    }

    pub(crate) fn observe(&mut self, observation: Observation) -> Decision {
        match observation {
            Observation::Line {
                instance,
                pid,
                stream,
                line,
                text,
            } => self.line(instance, pid, stream, line, text),
            Observation::UnexpectedExit { instance, detail } => {
                self.pending_exit
                    .get_or_insert(PendingExit { instance, detail });
                Decision {
                    stop: true,
                    reports: Vec::new(),
                }
            }
            Observation::Escape { cpu, pid, tid } => {
                self.escape(cpu, "outside allowed cpus", pid, tid)
            }
            Observation::Stall {
                instance,
                pid,
                cpu,
                detail,
            } => {
                let mut decision = Decision {
                    stop: true,
                    reports: Vec::new(),
                };
                if self.failure.is_none() && !self.escaped() {
                    let core = self.core(instance);
                    self.failure = Some(TrialFailure {
                        signal: Signal::Stall,
                        core,
                    });
                    decision.reports.push(Report::Signal {
                        core,
                        signal: Signal::Stall,
                        detail: detail.clone(),
                    });
                    decision.reports.push(Report::Warning(Sample {
                        warning: format!("stalled: {detail}"),
                        pid,
                        tid: pid,
                        cpu: CpuId(u16::try_from(cpu).unwrap_or(u16::MAX)),
                    }));
                }
                decision
            }
            Observation::LineTooLong { instance, stream } => {
                let core = self.core(instance);
                self.line_too_long = true;
                self.weak_reason(|| {
                    format!(
                        "core {core:02} read {}: backend output line exceeds 64 KiB",
                        stream.name()
                    )
                });
                Decision {
                    stop: true,
                    reports: Vec::new(),
                }
            }
            Observation::OutputError {
                instance,
                stream,
                error,
            } => {
                let core = self.core(instance);
                let message = format!("core {core:02} {}: {error}", stream.name());
                self.weak_reason(|| message.clone());
                self.diagnostics.push(message);
                Decision::default()
            }
            Observation::SamplingLoss { instance, what } => {
                let core = self.core(instance);
                self.weak_reason(|| format!("core {core:02} {what}"));
                Decision::default()
            }
            Observation::NeverEntered { instance, scope } => {
                let core = self.core(instance);
                self.weak_reason(|| {
                    format!("core {core:02} setup error: never entered scope {scope}")
                });
                Decision::default()
            }
        }
    }

    fn line(
        &mut self,
        instance: usize,
        pid: u32,
        stream: Stream,
        line: Line,
        text: String,
    ) -> Decision {
        let core = self.core(instance);
        let line = if stream == Stream::Stderr
            && SYSTEMD_STARTUP_FAILURES
                .iter()
                .any(|prefix| text.starts_with(prefix))
        {
            Line::SetupError
        } else {
            line
        };
        match line {
            Line::Other => Decision::default(),
            Line::Progress(detail) => Decision {
                stop: false,
                reports: if stream == Stream::Stdout {
                    let detail = if self.cores.len() > 1 {
                        format!("core {core:02} {detail}")
                    } else {
                        detail
                    };
                    vec![Report::Progress(detail)]
                } else {
                    Vec::new()
                },
            },
            Line::ComputationError => {
                let mut decision = Decision {
                    stop: true,
                    reports: Vec::new(),
                };
                let replaceable = self
                    .failure
                    .is_none_or(|selected| selected.signal != Signal::ComputationError);
                if !self.escaped() && replaceable {
                    self.failure = Some(TrialFailure {
                        signal: Signal::ComputationError,
                        core,
                    });
                    self.inconclusive = None;
                    decision.reports.push(Report::Signal {
                        core,
                        signal: Signal::ComputationError,
                        detail: text,
                    });
                }
                decision
            }
            Line::SetupError => {
                self.setup_error = true;
                let backend = self.backend;
                self.weak_reason(|| format!("{backend} setup error: {text}"));
                Decision {
                    stop: true,
                    reports: Vec::new(),
                }
            }
            Line::AffinityError(cpu) => {
                let cpu = cpu.unwrap_or(self.first_cpus[instance]);
                self.escape(cpu, "backend could not set affinity", pid, pid)
            }
        }
    }

    /// Settles what the exits seen before teardown mean, after final output was classified: an
    /// unexpected exit counts only when nothing else explains the trial.
    pub(crate) fn conclude(mut self) -> (Verdict, Vec<Report>) {
        let mut reports = Vec::new();
        if let Some(exit) = self.pending_exit.take() {
            if !self.line_too_long && !self.setup_error && !self.escaped() && self.failure.is_none()
            {
                let core = self.core(exit.instance);
                self.inconclusive = None;
                self.failure = Some(TrialFailure {
                    signal: Signal::UnexpectedExit,
                    core,
                });
                reports.push(Report::Signal {
                    core,
                    signal: Signal::UnexpectedExit,
                    detail: exit.detail,
                });
            }
        }
        (
            Verdict {
                failure: self.failure,
                inconclusive: self.inconclusive,
                escaped: self.escaped,
                diagnostics: self.diagnostics,
            },
            reports,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(n: u8) -> CoreId {
        CoreId::new(i64::from(n)).unwrap()
    }

    fn selector() -> Selector {
        Selector::new(Backend::Mprime, &[(core(3), CpuId(3))])
    }

    fn line(stream: Stream, line: Line, text: &str) -> Observation {
        Observation::Line {
            instance: 0,
            pid: 42,
            stream,
            line,
            text: text.to_owned(),
        }
    }

    fn compute() -> Observation {
        line(Stream::Stdout, Line::ComputationError, "COMPUTE ERROR")
    }

    fn setup() -> Observation {
        line(Stream::Stdout, Line::SetupError, "PIN FAILED")
    }

    fn affinity() -> Observation {
        line(
            Stream::Stderr,
            Line::AffinityError(Some(CpuId(9))),
            "AFFINITY:9",
        )
    }

    fn too_long() -> Observation {
        Observation::LineTooLong {
            instance: 0,
            stream: Stream::Stdout,
        }
    }

    fn conclude(selector: Selector) -> Verdict {
        selector.conclude().0
    }

    #[test]
    fn progress_is_reported_from_stdout_only() {
        let mut s = selector();
        assert_eq!(
            s.observe(line(Stream::Stdout, Line::Progress("ok".into()), "ok"))
                .reports,
            [Report::Progress("ok".into())]
        );
        assert!(
            s.observe(line(Stream::Stderr, Line::Progress("ok".into()), "ok"))
                .reports
                .is_empty()
        );
        assert!(
            s.observe(line(Stream::Watched, Line::Progress("ok".into()), "ok"))
                .reports
                .is_empty()
        );
    }

    #[test]
    fn progress_names_the_core_with_several_instances() {
        let mut s = Selector::new(Backend::Mprime, &[(core(1), CpuId(1)), (core(7), CpuId(7))]);
        let decision = s.observe(Observation::Line {
            instance: 1,
            pid: 1,
            stream: Stream::Stdout,
            line: Line::Progress("ok".into()),
            text: "ok".into(),
        });
        assert_eq!(decision.reports, [Report::Progress("core 07 ok".into())]);
    }

    #[test]
    fn computation_error_replaces_setup_error_in_either_order() {
        for order in [[setup(), compute()], [compute(), setup()]] {
            let mut s = selector();
            for observation in order {
                s.observe(observation);
            }
            let verdict = conclude(s);
            assert_eq!(
                verdict.failure.map(|f| f.signal),
                Some(Signal::ComputationError)
            );
            assert_eq!(verdict.inconclusive, None);
        }
    }

    #[test]
    fn affinity_error_wins_over_computation_error_in_either_order() {
        for order in [[compute(), affinity()], [affinity(), compute()]] {
            let mut s = selector();
            for observation in order {
                s.observe(observation);
            }
            let verdict = conclude(s);
            assert_eq!(verdict.escaped, [CpuId(9)]);
            assert_eq!(verdict.failure, None);
            assert_eq!(verdict.inconclusive, None);
        }
    }

    #[test]
    fn first_computation_core_stays_selected_and_is_reported_once() {
        let mut s = Selector::new(Backend::Mprime, &[(core(1), CpuId(1)), (core(2), CpuId(2))]);
        let first = s.observe(compute());
        assert_eq!(first.reports.len(), 1);
        let second = s.observe(Observation::Line {
            instance: 1,
            pid: 2,
            stream: Stream::Stdout,
            line: Line::ComputationError,
            text: "COMPUTE ERROR".into(),
        });
        assert!(second.stop && second.reports.is_empty());
        assert_eq!(conclude(s).failure.map(|f| f.core), Some(core(1)));
    }

    #[test]
    fn computation_error_replaces_an_earlier_stall_or_exit() {
        let mut s = selector();
        s.observe(Observation::Stall {
            instance: 0,
            pid: 4,
            cpu: 1,
            detail: "1.0s cpu in 10.0s running".into(),
        });
        s.observe(compute());
        assert_eq!(
            conclude(s).failure.map(|f| f.signal),
            Some(Signal::ComputationError)
        );
    }

    #[test]
    fn a_stall_reports_a_signal_and_a_warning() {
        let mut s = selector();
        let decision = s.observe(Observation::Stall {
            instance: 0,
            pid: 4,
            cpu: 2,
            detail: "1.0s cpu in 10.0s running".into(),
        });
        assert!(decision.stop);
        assert_eq!(decision.reports.len(), 2);
        assert!(matches!(
            &decision.reports[1],
            Report::Warning(sample) if sample.warning == "stalled: 1.0s cpu in 10.0s running"
                && sample.pid == 4 && sample.cpu == CpuId(2)
        ));
    }

    #[test]
    fn a_thread_escape_beats_everything_and_names_the_cpu() {
        let mut s = selector();
        s.observe(compute());
        s.observe(Observation::SamplingLoss {
            instance: 0,
            what: "thread sampling lost".into(),
        });
        let decision = s.observe(Observation::Escape {
            cpu: CpuId(5),
            pid: 10,
            tid: 11,
        });
        assert!(decision.stop);
        assert!(matches!(
            &decision.reports[0],
            Report::Warning(sample) if sample.warning == "outside allowed cpus" && sample.tid == 11
        ));
        s.observe(compute());
        let verdict = conclude(s);
        assert_eq!(verdict.escaped, [CpuId(5)]);
        assert_eq!((verdict.failure, verdict.inconclusive), (None, None));
    }

    #[test]
    fn sampling_loss_sticks_but_never_replaces_stronger_evidence() {
        let mut s = selector();
        s.observe(Observation::SamplingLoss {
            instance: 0,
            what: "usage sampling lost: boom".into(),
        });
        s.observe(Observation::SamplingLoss {
            instance: 0,
            what: "thread sampling lost: later".into(),
        });
        assert_eq!(
            conclude(s).inconclusive.as_deref(),
            Some("core 03 usage sampling lost: boom")
        );
        let mut s = selector();
        s.observe(compute());
        s.observe(Observation::SamplingLoss {
            instance: 0,
            what: "thread sampling lost".into(),
        });
        let verdict = conclude(s);
        assert_eq!(
            verdict.failure.map(|f| f.signal),
            Some(Signal::ComputationError)
        );
        assert_eq!(verdict.inconclusive, None);
    }

    #[test]
    fn an_affinity_error_without_a_readable_cpu_names_the_cpu_the_instance_was_given() {
        let mut s = selector();
        s.observe(line(
            Stream::Stderr,
            Line::AffinityError(None),
            "AFFINITY:x",
        ));
        assert_eq!(conclude(s).escaped, [CpuId(3)]);
    }

    #[test]
    fn the_cap_error_is_suppressed_by_stronger_evidence_in_both_orders() {
        for order in [[too_long(), compute()], [compute(), too_long()]] {
            let mut s = selector();
            for observation in order {
                s.observe(observation);
            }
            let verdict = conclude(s);
            assert_eq!(
                verdict.failure.map(|f| f.signal),
                Some(Signal::ComputationError)
            );
            assert_eq!(verdict.inconclusive, None);
        }
        for order in [[too_long(), affinity()], [affinity(), too_long()]] {
            let mut s = selector();
            for observation in order {
                s.observe(observation);
            }
            let verdict = conclude(s);
            assert_eq!(verdict.escaped, [CpuId(9)]);
            assert_eq!(verdict.inconclusive, None);
        }
    }

    #[test]
    fn the_cap_error_alone_makes_the_trial_inconclusive() {
        let mut s = selector();
        assert!(s.observe(too_long()).stop);
        let verdict = conclude(s);
        assert_eq!(
            verdict.inconclusive.as_deref(),
            Some("core 03 read stdout.log: backend output line exceeds 64 KiB")
        );
        assert!(verdict.diagnostics.is_empty());
    }

    #[test]
    fn an_ordinary_output_error_does_not_hide_a_later_early_exit() {
        let mut s = selector();
        s.observe(Observation::OutputError {
            instance: 0,
            stream: Stream::Stdout,
            error: "write output log: disk full".into(),
        });
        s.observe(Observation::UnexpectedExit {
            instance: 0,
            detail: "exit status 0".into(),
        });
        let verdict = conclude(s);
        assert_eq!(
            verdict.failure.map(|f| f.signal),
            Some(Signal::UnexpectedExit)
        );
        assert_eq!(verdict.inconclusive, None);
        assert_eq!(verdict.diagnostics.len(), 1);
    }

    #[test]
    fn ordinary_output_errors_always_surface_even_beside_strong_evidence() {
        let mut s = selector();
        s.observe(compute());
        s.observe(Observation::OutputError {
            instance: 0,
            stream: Stream::Stderr,
            error: "write output log: disk full".into(),
        });
        s.observe(too_long());
        let verdict = conclude(s);
        assert_eq!(
            verdict.failure.map(|f| f.signal),
            Some(Signal::ComputationError)
        );
        assert_eq!(
            verdict.diagnostics,
            ["core 03 stderr.log: write output log: disk full"]
        );
    }

    #[test]
    fn a_queued_exit_is_unexpected_unless_something_explains_it() {
        let exit = || Observation::UnexpectedExit {
            instance: 0,
            detail: "exit status 0".into(),
        };
        let mut s = selector();
        assert!(s.observe(exit()).stop);
        let (verdict, reports) = s.conclude();
        assert_eq!(
            verdict.failure.map(|f| f.signal),
            Some(Signal::UnexpectedExit)
        );
        assert_eq!(
            reports,
            [Report::Signal {
                core: core(3),
                signal: Signal::UnexpectedExit,
                detail: "exit status 0".into()
            }]
        );
        for blocker in [setup(), too_long(), compute(), affinity()] {
            let mut s = selector();
            s.observe(exit());
            s.observe(blocker);
            let (verdict, reports) = s.conclude();
            assert_ne!(
                verdict.failure.map(|f| f.signal),
                Some(Signal::UnexpectedExit)
            );
            assert!(reports.iter().all(|r| !matches!(
                r,
                Report::Signal {
                    signal: Signal::UnexpectedExit,
                    ..
                }
            )));
        }
    }

    #[test]
    fn an_unexpected_exit_outranks_and_clears_a_sampling_loss() {
        let mut s = selector();
        s.observe(Observation::SamplingLoss {
            instance: 0,
            what: "usage sampling lost: x".into(),
        });
        s.observe(Observation::UnexpectedExit {
            instance: 0,
            detail: "exit status 0".into(),
        });
        let verdict = conclude(s);
        assert_eq!(
            verdict.failure.map(|f| f.signal),
            Some(Signal::UnexpectedExit)
        );
        assert_eq!(verdict.inconclusive, None);
    }

    #[test]
    fn systemd_startup_text_on_stderr_is_a_setup_error_not_an_exit() {
        for text in [
            "Failed to start transient scope unit: Access denied",
            "Failed to execute /bin/x: No such file",
        ] {
            let mut s = selector();
            assert!(s.observe(line(Stream::Stderr, Line::Other, text)).stop);
            s.observe(Observation::UnexpectedExit {
                instance: 0,
                detail: "exit status 1".into(),
            });
            let verdict = conclude(s);
            assert_eq!(verdict.failure, None);
            assert_eq!(
                verdict.inconclusive.as_deref(),
                Some(&*format!("mprime setup error: {text}"))
            );
        }
        let mut s = selector();
        assert!(
            !s.observe(line(Stream::Stdout, Line::Other, "Failed to execute"))
                .stop
        );
    }

    #[test]
    fn a_scope_never_entered_is_a_weak_setup_observation() {
        let mut s = selector();
        s.observe(Observation::NeverEntered {
            instance: 0,
            scope: "togi-trial-x".into(),
        });
        assert_eq!(
            conclude(s).inconclusive.as_deref(),
            Some("core 03 setup error: never entered scope togi-trial-x")
        );
        let mut s = selector();
        s.observe(compute());
        s.observe(Observation::NeverEntered {
            instance: 0,
            scope: "togi-trial-x".into(),
        });
        assert_eq!(conclude(s).inconclusive, None);
    }
}
