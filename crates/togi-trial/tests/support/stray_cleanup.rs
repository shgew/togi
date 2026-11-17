use std::io;
use std::process::{Child, ExitStatus};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const STRAY_REAP: Duration = Duration::from_secs(5);

pub(crate) trait StrayChild: Send + 'static {
    fn kill(&mut self) -> io::Result<()>;
    fn try_wait(&mut self) -> io::Result<bool>;
    fn wait(&mut self) -> io::Result<()>;
}

impl StrayChild for Child {
    fn kill(&mut self) -> io::Result<()> {
        Child::kill(self)
    }

    fn try_wait(&mut self) -> io::Result<bool> {
        Child::try_wait(self).map(|status| status.is_some())
    }

    fn wait(&mut self) -> io::Result<()> {
        Child::wait(self).map(|_| ())
    }
}

pub(crate) struct StrayWaiter<C> {
    child: mpsc::Sender<C>,
    done: mpsc::Receiver<io::Result<()>>,
}

impl<C: StrayChild> StrayWaiter<C> {
    pub(crate) fn start() -> io::Result<Self> {
        let (child, waiting) = mpsc::channel::<C>();
        let (finished, done) = mpsc::channel();
        std::thread::Builder::new()
            .name("trial-test-reaper".to_owned())
            .spawn(move || {
                if let Ok(mut child) = waiting.recv() {
                    let _ = finished.send(child.wait());
                }
            })?;
        Ok(Self { child, done })
    }

    fn wait_until(self, child: C, deadline: Instant) -> io::Result<()> {
        self.child
            .send(child)
            .map_err(|_| io::Error::other("stray reaper disappeared"))?;
        self.done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    io::Error::new(io::ErrorKind::TimedOut, "stray reap deadline elapsed")
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    io::Error::other("stray reaper ended without reporting")
                }
            })?
    }
}

pub(crate) fn kill_stray<C: StrayChild>(
    mut child: C,
    waiter: StrayWaiter<C>,
    deadline: Instant,
) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(error) = child.kill() {
        errors.push(format!("kill stray child: {error}"));
    }
    match child.try_wait() {
        Ok(true) => return join_errors(&errors),
        Ok(false) => {}
        Err(error) => errors.push(format!("poll stray child: {error}")),
    }
    let reap = deadline.min(Instant::now() + STRAY_REAP);
    if let Err(error) = waiter.wait_until(child, reap) {
        errors.push(format!("reap stray child: {error}"));
    }
    join_errors(&errors)
}

pub(crate) fn join_errors(errors: &[String]) -> Result<(), String> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub(crate) fn isolated(
    what: &str,
    step: impl FnOnce(Instant) -> Result<(), String>,
    deadline: Instant,
) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| step(deadline))).unwrap_or_else(
        |payload| {
            let reason = payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-text panic".to_owned());
            Err(format!("{what} panicked: {reason}"))
        },
    )
}

pub(crate) fn cleanup_stray(
    deadline: Instant,
    kill: impl FnOnce(Instant) -> Result<(), String>,
    stop: impl FnOnce(Instant) -> Result<(), String>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(error) = isolated("stray kill", kill, deadline) {
        errors.push(error);
    }
    if let Err(error) = isolated("stray scope stop", stop, deadline) {
        errors.push(error);
    }
    join_errors(&errors)
}

pub(crate) const SYSTEMCTL_NOT_LOADED: i32 = 5;

pub(crate) fn stray_stop_error(
    scope: &str,
    stop: &Result<ExitStatus, String>,
    gone: bool,
) -> Option<String> {
    match stop {
        Ok(status) if status.success() => None,
        Ok(status) if gone && status.code() == Some(SYSTEMCTL_NOT_LOADED) => None,
        Ok(status) => Some(format!("systemctl stop {scope}: {status}")),
        Err(error) => Some(format!("systemctl stop {scope}: {error}")),
    }
}
