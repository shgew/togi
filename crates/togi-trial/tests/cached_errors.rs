#![cfg(all(feature = "hardware", target_os = "linux"))]

//! What an external caller of `Running::wait` can learn from a failed wait that it repeats: the
//! diagnostics the trial kept are reachable through the public error chain, and the categories of
//! cancellation stay what they were. These start real scopes, as the hardware tests do.

#[cfg(test)]
mod cached_errors {
    use std::error::Error as _;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
    use std::time::{Duration, Instant};

    use togi_backend::{Backend, Error, Line, Plan, Ready};
    use togi_hostlock::{DEFAULT_PATH, HostLock, Policy};
    use togi_machine::{
        CancellationToken, CcdId, Condition, CoreId, CoreInfo, CpuId, Partial, Regime, Reporter,
        Running, Sample, Signal, Stopped, TrialResult, TrialRole, TrialSpec, Trials, Workload,
        pick_workload,
    };
    use togi_trial::{BackendIdentity, Backends, Options, Runner};

    const CARRIER: &str = "trap 'head -c 70000 /dev/zero | tr \"\\000\" x; exit 0' TERM; \
                       : > \"$1\"; while :; do sleep 1 & wait $!; done";

    const PLAIN: &str = ": > \"$1\"; while :; do sleep 1 & wait $!; done";

    #[derive(Debug)]
    struct Shell {
        script: &'static str,
        marker: PathBuf,
    }

    impl Backend for Shell {
        fn configuration(&self, workload: &Workload) -> Result<Vec<u8>, Error> {
            Ok(workload.id.as_bytes().to_vec())
        }

        fn kind(&self) -> togi_machine::Backend {
            togi_machine::Backend::Mprime
        }

        fn check(&self) -> Result<Ready, Error> {
            Ok(Ready {
                detail: "shell".to_owned(),
            })
        }

        fn plan(&self, _: &Workload, _: &Path, _: &[CpuId]) -> Result<Plan, Error> {
            Ok(Plan {
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    self.script.into(),
                    "sh".into(),
                    self.marker.clone().into(),
                ],
                inputs: Vec::new(),
                watched: Vec::new(),
            })
        }

        fn classify(&self, _: &str) -> Line {
            Line::Other
        }
    }

    struct Quiet;

    impl Reporter for Quiet {
        fn progress(&mut self, _: &str) {}
        fn sample(&mut self, _: Sample) {}
        fn signal(&mut self, _: CoreId, _: Signal, _: Option<u32>, _: &str) {}
    }

    static SERIAL: Mutex<()> = Mutex::new(());

    /// The host lock when running as root, as on the target machine, where other tests sweep every
    /// scope; an ordinary account runs against its own user manager and needs none.
    struct Exclusive {
        _lock: Option<HostLock>,
        _serial: MutexGuard<'static, ()>,
    }

    fn exclusive() -> Exclusive {
        let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let lock = rustix::process::geteuid()
            .is_root()
            .then(|| HostLock::acquire(Path::new(DEFAULT_PATH), Policy::Host).unwrap());
        Exclusive {
            _lock: lock,
            _serial: serial,
        }
    }

    fn account() -> BackendIdentity {
        let uid = rustix::process::geteuid().as_raw();
        let gid = rustix::process::getegid().as_raw();
        BackendIdentity::new(uid, gid).expect("the caller is an ordinary account")
    }

    fn topology() -> Vec<CoreInfo> {
        vec![CoreInfo {
            core: CoreId::new(0).unwrap(),
            ccd: CcdId::new(0).unwrap(),
            cpus: vec![CpuId::from(2)],
        }]
    }

    fn spec(id: &str) -> TrialSpec {
        let workload = pick_workload(Regime::R1, 0).id();
        let cores = vec![CoreId::new(0).unwrap()];
        let cpus = vec![CpuId::from(2)];
        let roles = TrialRole::legacy(Regime::R1, workload, 1, &cores, &cpus, &topology()).unwrap();
        TrialSpec {
            id: id.into(),
            regime: Regime::R1,
            workload,
            condition: Condition::Alone,
            cores,
            cpus,
            duration: Duration::from_secs(600),
            index: 0,
            seed: 1,
            offsets: togi_machine::PerCore::default(),
            roles,
            controller: togi_machine::ControllerMode::Managed {
                rotation: 0,
                purpose: togi_machine::Purpose::Exact,
            },
        }
    }

    fn started(runner: &mut Runner, id: &str) -> Box<dyn Running> {
        #[expect(
            clippy::disallowed_methods,
            reason = "a real-process test stands in for the session's proof that nothing runs"
        )]
        let ready = Stopped::confirmed();
        runner
            .start(&CancellationToken::new(), spec(id), ready)
            .map_err(|partial| partial.error)
            .unwrap()
    }

    fn armed(marker: &Path) -> bool {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if marker.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        marker.exists()
    }

    fn kept_error(error: &togi_machine::Error) -> Option<&togi_trial::Error> {
        let mut link: Option<&(dyn std::error::Error + 'static)> = Some(error);
        while let Some(cause) = link {
            if let Some(found) = cause.downcast_ref::<togi_trial::Error>() {
                return Some(found);
            }
            link = cause.source();
        }
        None
    }

    fn cancelled_wait(trial: &mut dyn Running) -> Partial<TrialResult> {
        let cancel = CancellationToken::new();
        cancel.cancel();
        trial.wait(&cancel, &mut Quiet).unwrap_err()
    }

    #[test]
    fn real_process_repeated_waits_keep_the_joined_diagnostics_inspectable_from_outside() {
        let _exclusive = exclusive();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ready");
        #[expect(
            clippy::disallowed_methods,
            reason = "real-process error-chain fixtures use disposable output without durability"
        )]
        let mut runner = Runner::new_unsynced(Options {
            dir: dir.path().join("trials"),
            backends: Backends::new().with(Arc::new(Shell {
                script: CARRIER,
                marker: marker.clone(),
            })),
            topology: topology(),
            identity: Some(account()),
            pm_table: None,
        })
        .unwrap();
        let mut trial = started(&mut runner, "cachedjoined");
        assert!(armed(&marker), "the workload never installed its handler");
        let first = cancelled_wait(trial.as_mut());
        let second = cancelled_wait(trial.as_mut());
        let mut kept = Vec::new();
        for partial in [&first, &second] {
            let Some(togi_trial::Error::Joined(joined)) = kept_error(&partial.error) else {
                panic!(
                    "the kept error is unreachable through the chain: {:?}",
                    partial.error
                );
            };
            assert!(
                matches!(
                    joined.errors(),
                    [togi_trial::Error::Cancelled, togi_trial::Error::Output(output)]
                        if !output.diagnostics().is_empty()
                ),
                "{joined:?}"
            );
            kept.push(kept_error(&partial.error).unwrap());
        }
        assert!(
            std::ptr::eq(kept[0], kept[1]),
            "every wait reports the one kept error"
        );
        assert_eq!(first.error.to_string(), second.error.to_string());
        assert!(trial.stop().is_ok());
    }

    #[test]
    fn real_process_a_plain_cancellation_stays_cancelled_on_every_wait() {
        let _exclusive = exclusive();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ready");
        #[expect(
            clippy::disallowed_methods,
            reason = "real-process cancellation fixtures use disposable output without durability"
        )]
        let mut runner = Runner::new_unsynced(Options {
            dir: dir.path().join("trials"),
            backends: Backends::new().with(Arc::new(Shell {
                script: PLAIN,
                marker: marker.clone(),
            })),
            topology: topology(),
            identity: Some(account()),
            pm_table: None,
        })
        .unwrap();
        let mut trial = started(&mut runner, "cachedcancel");
        assert!(armed(&marker), "the workload never started");
        for _ in 0..2 {
            let partial = cancelled_wait(trial.as_mut());
            assert!(
                matches!(partial.error, togi_machine::Error::Cancelled),
                "{:?}",
                partial.error
            );
            assert!(partial.error.source().is_none());
        }
        assert!(trial.stop().is_ok());
    }
}
