use std::rc::Rc;

use std::collections::BTreeSet;
use togi_machine::Stopped;
use tokio::sync::mpsc::Receiver;
use tokio::task::JoinSet;

use tokio::time::{Instant, sleep, sleep_until, timeout_at};

use crate::error::{CleanupFailure, Containment};
use crate::evidence::Stream;
use crate::id::ScopeName;
use crate::instance::{Flow, Inbound, LiveInstance, dispatch, poll_watched};
use crate::judge::Judge;
use crate::os::{Child as _, GroupSignal, KillSignal, Os, SignalError, Systemd};
use crate::timing::Timing;
use crate::watch::{Watched, WritersStopped};

/// One absolute deadline for the whole teardown; every phase takes a slice of what remains.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    deadline: Instant,
    timing: Timing,
}

impl Budget {
    pub(crate) fn start(timing: Timing) -> Self {
        Self {
            deadline: Instant::now() + timing.teardown,
            timing,
        }
    }

    pub(crate) const fn deadline(&self) -> Instant {
        self.deadline
    }

    fn slice(&self, length: std::time::Duration) -> Instant {
        self.deadline.min(Instant::now() + length)
    }

    /// End of the shared SIGTERM grace.
    fn grace_end(&self) -> Instant {
        self.slice(self.timing.stop_grace)
    }

    /// End of the one slot every scope kill shares.
    fn kill_end(&self) -> Instant {
        self.slice(self.timing.kill_slot)
    }

    /// End of the output drain.
    fn drain_end(&self) -> Instant {
        self.slice(self.timing.drain)
    }
}

/// Failures in the order met; the first is never replaced.
#[derive(Debug, Default)]
pub(crate) struct Failures(Option<Containment>);

impl Failures {
    pub(crate) fn record(&mut self, failure: CleanupFailure) {
        match &mut self.0 {
            Some(containment) => containment.push(failure),
            None => self.0 = Some(Containment::new(failure)),
        }
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) fn into_option(self) -> Option<Containment> {
        self.0
    }
}

/// The trial's launched instances plus every scope attempted, also those of a failed launch.
pub(crate) struct Live<O: Os> {
    pub(crate) instances: Vec<LiveInstance<O>>,
    pub(crate) scopes: Vec<ScopeName>,
    /// Whether the instances run in scopes whose kill proves their writers stopped.
    pub(crate) scoped: bool,
    pub(crate) failures: Failures,
}

pub(crate) struct Running;
pub(crate) struct Reaped;
pub(crate) struct ReadersDone;
pub(crate) struct Verified;

const VERIFY_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// The teardown typestate: each step consumes the last, so the proof can only come from the end.
pub(crate) struct Teardown<'a, O: Os, S> {
    os: &'a Rc<O>,
    inbound: &'a mut Receiver<Inbound>,
    judge: &'a mut Judge,
    budget: Budget,
    live: Live<O>,
    failures: Failures,
    rotation: usize,
    _state: S,
}

/// What a finished teardown leaves: the proof, or every failure that withholds it.
pub(crate) struct Ended {
    pub(crate) proof: Result<Stopped, Containment>,
}

fn text_says_missing(text: &str) -> bool {
    text.contains("not loaded") || text.contains("could not be found")
}

impl<'a, O: Os, S> Teardown<'a, O, S> {
    fn advance<T>(self, state: T) -> Teardown<'a, O, T> {
        Teardown {
            os: self.os,
            inbound: self.inbound,
            judge: self.judge,
            budget: self.budget,
            live: self.live,
            failures: self.failures,
            rotation: self.rotation,
            _state: state,
        }
    }

    fn handle(&mut self, event: Inbound) -> Flow {
        dispatch(
            self.judge,
            &mut self.live.instances,
            &mut self.failures,
            event,
        )
    }

    /// Handles inbound events until `done` holds or `end` passes.
    async fn pump_until(&mut self, end: Instant, done: impl Fn(&[LiveInstance<O>]) -> bool) {
        while !done(&self.live.instances) {
            tokio::select! {
                event = self.inbound.recv() => match event {
                    Some(event) => { self.handle(event); }
                    None => break,
                },
                () = sleep_until(end) => break,
            }
        }
    }
}

impl<'a, O: Os> Teardown<'a, O, Running> {
    pub(crate) fn begin(
        os: &'a Rc<O>,
        inbound: &'a mut Receiver<Inbound>,
        judge: &'a mut Judge,
        mut live: Live<O>,
        budget: Budget,
    ) -> Self {
        let failures = std::mem::take(&mut live.failures);
        Teardown {
            os,
            inbound,
            judge,
            budget,
            live,
            failures,
            rotation: 0,
            _state: Running,
        }
    }

    /// SIGCONT then SIGTERM to each group with a shared grace, SIGKILL through every known
    /// scope in one slot, SIGKILL to each group, then reap every leader.
    pub(crate) async fn reap(mut self) -> Teardown<'a, O, Reaped> {
        self.grace().await;
        let confirmed = self.kill_scopes().await;
        self.kill_groups(&confirmed);
        self.reap_leaders().await;
        self.advance(Reaped)
    }

    async fn grace(&mut self) {
        for instance in &self.live.instances {
            let _ = instance.signal(GroupSignal::Cont);
            let _ = instance.signal(GroupSignal::Term);
        }
        let end = self.budget.grace_end();
        self.pump_until(end, |instances| instances.iter().all(LiveInstance::exited))
            .await;
    }

    /// Kills every known scope concurrently inside one shared slot; returns the scopes whose
    /// death is confirmed, which includes a scope that is already gone.
    async fn kill_scopes(&mut self) -> Vec<ScopeName> {
        let end = self.budget.kill_end();
        let mut scopes = self.live.scopes.clone();
        scopes.sort();
        scopes.dedup();
        let mut calls = JoinSet::new();
        for scope in scopes {
            let os = Rc::clone(self.os);
            calls.spawn_local(async move {
                let operation = Systemd::Kill {
                    scope: scope.clone(),
                    signal: KillSignal::Kill,
                };
                let outcome = timeout_at(end, os.systemd(&operation, end)).await;
                (scope, outcome)
            });
        }
        let mut confirmed = Vec::new();
        while let Some(joined) = calls.join_next().await {
            let Ok((scope, outcome)) = joined else {
                continue;
            };
            let failure = match outcome {
                Ok(Ok(output)) if output.status.success() => None,
                Ok(Ok(output)) => {
                    let text = format!(
                        "{}{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    (!text_says_missing(&text)).then(|| {
                        Box::<dyn std::error::Error + Send + Sync>::from(format!(
                            "systemctl kill exited with {}: {}",
                            output.status,
                            text.trim()
                        ))
                    })
                }
                Ok(Err(error)) => Some(Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
                Err(elapsed) => Some(Box::new(elapsed) as Box<dyn std::error::Error + Send + Sync>),
            };
            match failure {
                None => confirmed.push(scope),
                Some(source) => self.failures.record(CleanupFailure::KillScope {
                    scope: scope.to_string(),
                    signal: KillSignal::Kill.name(),
                    source,
                }),
            }
        }
        confirmed
    }

    /// SIGKILL to each group; decides per instance whether its writers are confirmed stopped.
    fn kill_groups(&mut self, confirmed: &[ScopeName]) {
        let scoped = self.live.scoped;
        for instance in &mut self.live.instances {
            let scope_confirmed = confirmed.contains(&instance.scope);
            let writers_stopped = match instance.signal(GroupSignal::Kill) {
                Ok(()) => !scoped || scope_confirmed,
                Err(SignalError::Gone) => scoped && scope_confirmed,
                Err(SignalError::Failed(source)) => {
                    self.failures.record(CleanupFailure::SignalGroup {
                        pgid: i32::try_from(instance.pid).unwrap_or(i32::MAX),
                        signal: GroupSignal::Kill.name(),
                        source,
                    });
                    false
                }
            };
            instance.writers = writers_stopped.then(WritersStopped::confirmed);
        }
    }

    async fn reap_leaders(&mut self) {
        let end = self.budget.deadline();
        self.pump_until(end, |instances| {
            instances
                .iter()
                .all(|instance| instance.child.is_none() || instance.exited())
        })
        .await;
        for instance in &mut self.live.instances {
            let Some(child) = instance.child.take() else {
                continue;
            };
            let label = instance.plan.name.dir_name();
            let reaped = if instance.exited() {
                Rc::try_unwrap(child)
                    .map_err(|_| std::io::Error::other("the leader is still referenced"))
                    .and_then(|child| child.reap().map(drop))
            } else {
                instance.waiter.abort();
                Err(std::io::Error::other("the leader did not exit"))
            };
            if let Err(source) = reaped {
                self.failures.record(CleanupFailure::Reap {
                    instance: label,
                    source,
                });
            }
        }
    }
}

impl<'a, O: Os> Teardown<'a, O, Reaped> {
    /// Drains pipes and watched files until the drain slot ends; both readers must hit end of
    /// input, and the drain keeps polling files fairly so a flood cannot starve another file.
    pub(crate) async fn drain(mut self) -> Teardown<'a, O, ReadersDone> {
        let end = self.budget.drain_end();
        let mut timed_out = false;
        loop {
            let mut pending = false;
            let count = self.live.instances.len();
            for step in 0..count {
                let index = (self.rotation + step) % count;
                let polled = poll_watched(
                    &mut self.live.instances[index],
                    self.judge,
                    &mut self.failures,
                    &mut || Instant::now() >= end,
                );
                pending |= polled.pending;
            }
            self.rotation = self.rotation.wrapping_add(1);
            let readers_open = self
                .live
                .instances
                .iter()
                .any(|instance| !instance.readers_closed());
            if !pending && !readers_open {
                break;
            }
            if Instant::now() >= end {
                timed_out = true;
                break;
            }
            if pending {
                while let Ok(event) = self.inbound.try_recv() {
                    self.handle(event);
                }
                tokio::task::yield_now().await;
            } else {
                tokio::select! {
                    event = self.inbound.recv() => match event {
                        Some(event) => { self.handle(event); }
                        None => break,
                    },
                    () = sleep_until(end) => {}
                }
            }
        }
        for instance in &mut self.live.instances {
            for reader in &instance.readers {
                if !reader.closed {
                    reader.task.abort();
                }
            }
        }
        let unfinished = self.live.instances.iter().any(|instance| {
            !instance.readers_closed() || instance.watched.iter().any(|file| !file.drained())
        });
        if timed_out || unfinished {
            self.failures.record(CleanupFailure::DrainDeadline);
        }
        self.advance(ReadersDone)
    }
}

impl<'a, O: Os> Teardown<'a, O, ReadersDone> {
    /// A scope kill acknowledges delivery, not exit, so a descendant can outlive it: scans `/proc`
    /// until no process remains in any known scope, within the absolute deadline. Writers count as
    /// stopped only for instances whose scope was seen empty.
    pub(crate) async fn verify(mut self) -> Teardown<'a, O, Verified> {
        if !self.live.scoped || self.live.scopes.is_empty() {
            return self.advance(Verified);
        }
        let deadline = self.budget.deadline();
        let remaining = loop {
            let left = match self.os.scope_processes(deadline).await {
                Ok(members) => members
                    .into_iter()
                    .map(|member| member.scope)
                    .filter(|scope| self.live.scopes.contains(scope))
                    .collect::<BTreeSet<_>>(),
                Err(error) => {
                    self.failures.record(CleanupFailure::Observe {
                        what: "the trial scope members",
                        source: Box::new(error),
                    });
                    break None;
                }
            };
            let late = Instant::now() > deadline;
            if late && left.is_empty() {
                self.failures.record(CleanupFailure::Observe {
                    what: "the trial scope members",
                    source: "the scan finished after the teardown deadline".into(),
                });
                break None;
            }
            if left.is_empty() || late || Instant::now() >= deadline {
                break Some(left);
            }
            tokio::select! {
                () = sleep(VERIFY_POLL) => {}
                () = sleep_until(deadline) => {}
            }
        };
        match remaining {
            Some(left) => {
                for scope in &left {
                    self.failures.record(CleanupFailure::ScopeMembers {
                        scope: scope.to_string(),
                    });
                }
                for instance in &mut self.live.instances {
                    if left.contains(&instance.scope) {
                        instance.writers = None;
                    }
                }
            }
            None => {
                for instance in &mut self.live.instances {
                    instance.writers = None;
                }
            }
        }
        self.advance(Verified)
    }
}

impl<O: Os> Teardown<'_, O, Verified> {
    /// Classifies the final partial lines of watched files whose writers are confirmed stopped,
    /// then mints the proof; this is the only place that does.
    pub(crate) fn finish(mut self) -> Ended {
        for instance in &mut self.live.instances {
            let (index, pid) = (instance.index, instance.pid);
            if let Some(writers) = instance.writers {
                for file in &mut instance.watched {
                    if let Some(line) = file.finish(writers) {
                        self.judge.line(index, pid, Stream::Watched, line);
                    }
                }
            }
        }
        let complete = self.live.instances.iter().all(|instance| {
            instance.child.is_none()
                && instance.readers_closed()
                && instance.writers.is_some()
                && instance.watched.iter().all(Watched::drained)
        });
        if !complete && self.failures.is_empty() {
            self.failures.record(CleanupFailure::DrainDeadline);
        }
        let proof = match self.failures.into_option() {
            None => {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "teardown reached Ended: every leader reaped, every reader at end of input, every scope seen empty"
                )]
                let stopped = Stopped::confirmed();
                Ok(stopped)
            }
            Some(containment) => Err(containment),
        };
        Ended { proof }
    }
}
