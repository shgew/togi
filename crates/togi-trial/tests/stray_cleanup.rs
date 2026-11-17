#![cfg(unix)]

#[path = "support/stray_cleanup.rs"]
mod fixture;

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::fixture::{
        SYSTEMCTL_NOT_LOADED, StrayChild, StrayWaiter, cleanup_stray, kill_stray, stray_stop_error,
    };

    #[test]
    fn stray_cleanup_accepts_an_unloaded_scope_only_once_it_is_gone() {
        use std::os::unix::process::ExitStatusExt as _;
        let exited = |code: i32| Ok(std::process::ExitStatus::from_raw(code << 8));
        let scope = "togi-unrelated-hardware.scope";
        assert_eq!(stray_stop_error(scope, &exited(0), true), None);
        assert_eq!(
            stray_stop_error(scope, &exited(SYSTEMCTL_NOT_LOADED), true),
            None
        );
        let unloaded_but_listed = stray_stop_error(scope, &exited(SYSTEMCTL_NOT_LOADED), false);
        assert!(
            unloaded_but_listed
                .as_deref()
                .is_some_and(|error| error.contains("exit status: 5")),
            "{unloaded_but_listed:?}"
        );
        let refused = stray_stop_error(scope, &exited(1), true);
        assert!(
            refused
                .as_deref()
                .is_some_and(|error| error.contains("exit status: 1")),
            "{refused:?}"
        );
        let unrun = stray_stop_error(scope, &Err("injected spawn failure".to_owned()), true);
        assert!(
            unrun
                .as_deref()
                .is_some_and(|error| error.contains("injected spawn failure")),
            "{unrun:?}"
        );
    }

    struct ReapFault {
        caller: std::thread::ThreadId,
        events: mpsc::Sender<&'static str>,
        released: mpsc::Receiver<()>,
        kill_fails: bool,
        poll_fails: bool,
    }

    impl StrayChild for ReapFault {
        fn kill(&mut self) -> io::Result<()> {
            let _ = self.events.send("kill");
            if self.kill_fails {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected kill refusal",
                ))
            } else {
                Ok(())
            }
        }

        fn try_wait(&mut self) -> io::Result<bool> {
            let _ = self.events.send("try_wait");
            if self.poll_fails {
                Err(io::Error::other("injected reap failure"))
            } else {
                Ok(false)
            }
        }

        fn wait(&mut self) -> io::Result<()> {
            if std::thread::current().id() == self.caller {
                let _ = self.events.send("unbounded wait");
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "injected blocked child wait",
                ));
            }
            self.released
                .recv_timeout(Duration::from_secs(1))
                .map_err(io::Error::other)
        }
    }

    #[test]
    fn stray_cleanup_stops_the_scope_after_a_reap_timeout() {
        let (record, events) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let child = ReapFault {
            caller: std::thread::current().id(),
            events: record.clone(),
            released,
            kill_fails: false,
            poll_fails: false,
        };
        let waiter = StrayWaiter::start().unwrap();
        let deadline = Instant::now();
        let result = cleanup_stray(
            deadline,
            |bound| kill_stray(child, waiter, bound),
            |bound| {
                assert_eq!(bound, deadline);
                let _ = record.send("stop");
                let _ = release.send(());
                Ok(())
            },
        );
        assert_eq!(
            events.try_iter().collect::<Vec<_>>(),
            ["kill", "try_wait", "stop"]
        );
        assert!(result.unwrap_err().contains("stray reap deadline elapsed"));
    }

    #[test]
    fn stray_cleanup_preserves_kill_reap_and_scope_failures() {
        let (record, events) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let child = ReapFault {
            caller: std::thread::current().id(),
            events: record.clone(),
            released,
            kill_fails: true,
            poll_fails: true,
        };
        let waiter = StrayWaiter::start().unwrap();
        let result = cleanup_stray(
            Instant::now(),
            |bound| kill_stray(child, waiter, bound),
            |_| {
                let _ = record.send("stop");
                let _ = release.send(());
                Err("injected scope stop failure".to_owned())
            },
        );
        assert_eq!(
            events.try_iter().collect::<Vec<_>>(),
            ["kill", "try_wait", "stop"]
        );
        let error = result.unwrap_err();
        let kill = error.find("injected kill refusal");
        let reap = error.find("injected reap failure");
        let stop = error.find("injected scope stop failure");
        assert!(
            kill.is_some() && reap.is_some() && stop.is_some(),
            "{error}"
        );
        assert!(kill < reap && reap < stop, "{error}");
    }

    #[test]
    fn stray_cleanup_stops_the_scope_after_a_kill_panic() {
        let (record, events) = mpsc::channel();
        let result = cleanup_stray(
            Instant::now(),
            |_| panic!("injected kill panic"),
            |_| {
                let _ = record.send("stop");
                panic!("injected stop panic")
            },
        );
        assert_eq!(events.try_iter().collect::<Vec<_>>(), ["stop"]);
        let error = result.unwrap_err();
        let kill = error.find("injected kill panic");
        let stop = error.find("injected stop panic");
        assert!(kill.is_some() && stop.is_some(), "{error}");
        assert!(kill < stop, "{error}");
    }
}
