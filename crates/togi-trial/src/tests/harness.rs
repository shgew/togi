//! A trial runner over the fake OS under paused time.

use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::task::LocalSet;
use togi_machine::{
    CancellationToken, CcdId, Condition, CoreId, CoreInfo, CpuId, Partial, Regime, Stopped,
    TrialSpec, pick_workload,
};

use crate::identity::BackendIdentity;
use crate::os::fake::Fake;
use crate::runner::Backends;
use crate::sensors::Roots;
use crate::supervisor::{Env, Event, Finished, Handle, start};
use crate::testing::TestBackend;
use crate::timing::Timing;

pub(super) fn core(n: u8) -> CoreId {
    CoreId::new(i64::from(n)).unwrap()
}

/// Cores 0..16; core n sits on CCD n/8 with logical CPUs n and n + 32.
pub(super) fn topology() -> Vec<CoreInfo> {
    (0..16)
        .map(|n| CoreInfo {
            core: core(n),
            ccd: CcdId(n / 8),
            cpus: vec![CpuId(u16::from(n)), CpuId(u16::from(n) + 32)],
        })
        .collect()
}

pub(super) fn spec(regime: Regime, cores: &[u8], duration: Duration) -> TrialSpec {
    let topology = topology();
    let cpus = if regime.instance_per_core() {
        cores.iter().map(|&n| topology[usize::from(n)].cpus[0]).collect()
    } else if regime == Regime::R5 {
        topology[usize::from(cores[0])].cpus.clone()
    } else {
        vec![topology[usize::from(cores[0])].cpus[0]]
    };
    TrialSpec {
        id: "t1".into(),
        regime,
        workload: pick_workload(regime, 0).clone(),
        condition: Condition::Alone,
        cores: cores.iter().map(|&n| core(n)).collect(),
        cpus,
        duration,
        index: 0,
        seed: 7,
    }
}

pub(super) struct Rig {
    pub(super) fake: Fake,
    pub(super) env: Rc<Env<Fake>>,
    pub(super) dir: TempDir,
}

pub(super) fn own_identity() -> BackendIdentity {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(".").unwrap();
    BackendIdentity::new(meta.uid(), meta.gid()).unwrap()
}

pub(super) fn rig_with(backend: TestBackend, timing: Timing) -> Rig {
    rig_edit(backend, timing, |_| {})
}

pub(super) fn rig_edit(
    backend: TestBackend,
    timing: Timing,
    edit: impl FnOnce(&mut Env<Fake>),
) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let roots = Roots {
        hwmon: dir.path().join("hwmon"),
        cpu: dir.path().join("cpu"),
        powercap: dir.path().join("powercap"),
        proc: dir.path().join("proc"),
    };
    let mut env = Env {
        os: Rc::new(fake.clone()),
        timing,
        roots,
        dir: dir.path().join("trials"),
        backends: Backends::new().with(Arc::new(backend)),
        topology: topology(),
        identity: Some(own_identity()),
        unscoped: false,
        user_manager: false,
        pm_table: None,
        storage: None,
    };
    edit(&mut env);
    let env = Rc::new(env);
    Rig { fake, env, dir }
}

pub(super) fn rig() -> Rig {
    rig_with(TestBackend::new("/bin/true"), Timing::default())
}

/// A proof that nothing runs, for tests; the production crate mints it only after a teardown.
pub(super) fn nothing_runs() -> Stopped {
    #[expect(
        clippy::disallowed_methods,
        reason = "a test stands in for the session's proof that nothing runs"
    )]
    Stopped::confirmed()
}

impl Rig {
    pub(super) async fn start(&self, spec: TrialSpec) -> Result<Handle, Partial<Option<Stopped>>> {
        start(
            Rc::clone(&self.env),
            CancellationToken::new(),
            spec,
            nothing_runs(),
        )
        .await
    }

    pub(super) async fn start_with(
        &self,
        cancel: &CancellationToken,
        spec: TrialSpec,
    ) -> Result<Handle, Partial<Option<Stopped>>> {
        start(Rc::clone(&self.env), cancel.clone(), spec, nothing_runs()).await
    }
}

/// What a trial reported on its way to the end.
pub(super) struct Run {
    pub(super) events: Vec<Event>,
    pub(super) finished: Finished,
}

impl Run {
    pub(super) fn progress(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Progress(detail) => Some(detail.as_str()),
                _ => None,
            })
            .collect()
    }

    pub(super) fn signals(&self) -> Vec<(CoreId, togi_machine::Signal)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Signal { core, signal, .. } => Some((*core, *signal)),
                _ => None,
            })
            .collect()
    }

    pub(super) fn warnings(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Sample(sample) => Some(sample.warning.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Lets the supervisor run, in virtual time, until it reports its end.
pub(super) async fn finish(handle: &Handle) -> Run {
    let mut events = Vec::new();
    loop {
        match handle.events.try_recv() {
            Ok(Event::Finished(finished)) => {
                return Run {
                    events,
                    finished: *finished,
                };
            }
            Ok(event) => events.push(event),
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
}

pub(super) async fn local<F: Future>(test: F) -> F::Output {
    LocalSet::new().run_until(test).await
}

use crate::os::fake::{Launch, ProcCtx, ProcShared, script};
use crate::procfs::Usage;

/// A process that prints `lines` on stdout once, then runs until killed.
pub(super) fn printing(lines: &'static str) -> impl FnMut(usize, &crate::launch::LaunchCommand) -> Launch {
    move |_, _| {
        Launch::Run(script(move |mut ctx: ProcCtx| async move {
            ctx.print(lines).await;
            ctx.until_killed().await;
        }))
    }
}

/// Makes the process burn `cpus` CPUs worth of CPU time, reported every second.
pub(super) fn burn(shared: &std::rc::Rc<ProcShared>, cpus: u32) {
    let shared = std::rc::Rc::clone(shared);
    tokio::task::spawn_local(async move {
        let mut total = Duration::ZERO;
        while !shared.exited() {
            tokio::time::sleep(Duration::from_secs(1)).await;
            total += Duration::from_secs(1) * cpus;
            *shared.usage.borrow_mut() = Ok(Usage {
                cpu_time: total,
                cpu: 0,
            });
        }
    });
}

/// The crate's own error inside a seam error, when it carries one.
pub(super) fn crate_error(error: &togi_machine::Error) -> Option<&crate::Error> {
    match error {
        togi_machine::Error::Other(inner) => inner.downcast_ref::<crate::Error>(),
        _ => None,
    }
}
