use crate::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

#[test]
fn descendant_observes_cancellation_after_an_intermediate_token_is_dropped() {
    let parent = CancellationToken::new();
    let descendant = parent.child_token().child_token().child_token();
    assert!(!descendant.is_cancelled());
    parent.cancel();
    assert!(descendant.is_cancelled());
    assert!(descendant.wait_timeout(std::time::Duration::ZERO));
}

#[test]
fn waiter_on_descendant_wakes_when_an_ancestor_is_cancelled_after_intermediates_drop() {
    let parent = CancellationToken::new();
    let descendant = parent.child_token().child_token();
    let (started, ready) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        started.send(()).unwrap();
        descendant.wait();
        descendant.is_cancelled()
    });
    ready.recv().unwrap();
    parent.cancel();
    assert!(waiter.join().unwrap());
}

#[test]
fn cancelling_a_dropped_intermediate_chain_leaves_siblings_alone() {
    let parent = CancellationToken::new();
    let sibling = parent.child_token();
    let descendant = parent.child_token().child_token();
    descendant.cancel();
    assert!(!sibling.is_cancelled());
    assert!(!parent.is_cancelled());
    parent.cancel();
    assert!(sibling.is_cancelled());
}

fn result(ran: Duration) -> TrialResult {
    TrialResult {
        ran,
        failure: None,
        inconclusive: None,
        escaped: Vec::new(),
        tctl_max_c: None,
        stops: 0,
        conts: 0,
    }
}

struct FailingRunning;

impl Running for FailingRunning {
    fn started(&self) -> Started {
        Started::default()
    }

    fn wait(
        &mut self,
        _cancel: &CancellationToken,
        _report: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        Err(Partial {
            value: result(Duration::from_secs(85)),
            error: Error::Cancelled,
        })
    }

    fn stop(self: Box<Self>) -> Result<Stopped, Error> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the test double stands for a seam that confirmed teardown"
        )]
        Ok(Stopped::confirmed())
    }
}

struct NoReporter;

impl Reporter for NoReporter {
    fn progress(&mut self, _detail: &str) {}
    fn sample(&mut self, _sample: Sample) {}
    fn signal(&mut self, _core: CoreId, _signal: Signal, _detail: &str) {}
}

fn propagate(running: &mut dyn Running) -> Result<TrialResult, Box<dyn std::error::Error>> {
    Ok(running.wait(&CancellationToken::new(), &mut NoReporter)?)
}

#[test]
fn partial_propagates_with_question_mark_and_keeps_its_error() {
    let error = propagate(&mut FailingRunning).unwrap_err();
    assert_eq!(
        crate::error_chain(&*error),
        "machine operation left partial progress: cancelled"
    );
}

#[test]
fn partial_keeps_its_named_error_findable_after_question_mark_into_a_boxed_error() {
    fn propagate_boxed(
        running: &mut dyn Running,
    ) -> Result<TrialResult, Box<dyn std::error::Error + Send + Sync>> {
        Ok(running.wait(&CancellationToken::new(), &mut NoReporter)?)
    }
    let error = propagate_boxed(&mut FailingRunning).unwrap_err();
    let chain = std::iter::successors(Some(&*error as &(dyn std::error::Error + 'static)), |e| {
        e.source()
    });
    let named = chain.into_iter().find_map(|e| e.downcast_ref::<Error>());
    assert!(matches!(named, Some(Error::Cancelled)), "{named:?}");
}

#[test]
fn partial_names_its_progress_and_retains_its_error() {
    let partial = Partial {
        value: 7_u8,
        error: Error::from(Box::<dyn std::error::Error + Send + Sync>::from(
            "disk gone",
        )),
    };
    assert_eq!(
        partial.to_string(),
        "machine operation left partial progress"
    );
    assert_eq!(
        crate::error_chain(&partial),
        "machine operation left partial progress: disk gone"
    );
    assert_eq!(
        std::error::Error::source(&partial).map(ToString::to_string),
        Some("disk gone".to_owned())
    );
    let (value, error) = partial.into_parts();
    assert_eq!(value, 7);
    assert_eq!(error.to_string(), "disk gone");
    let wrapped = Partial {
        value: (),
        error: Error::from(Box::<dyn std::error::Error + Send + Sync>::from(
            std::io::Error::other(std::io::Error::other("inner")),
        )),
    };
    assert_eq!(
        std::error::Error::source(&wrapped)
            .and_then(std::error::Error::source)
            .map(ToString::to_string),
        std::error::Error::source(&wrapped.error).map(ToString::to_string)
    );
}

#[test]
fn ids_serialize_as_plain_strings() {
    let boot = BootId::from("b".to_owned());
    assert_eq!(serde_json::to_string(&boot).unwrap(), "\"b\"");
    assert_eq!(
        serde_json::from_str::<KernelCursor>("\"c\"")
            .unwrap()
            .to_string(),
        "c"
    );
}

#[test]
fn reset_reason_tells_no_line_from_an_unrecognized_one() {
    let silent = ResetReason {
        kind: None,
        raw: Vec::new(),
        supported: true,
    };
    let unrecognized = ResetReason {
        kind: Some(ResetKind::Unknown),
        raw: vec!["something new".to_owned()],
        supported: true,
    };
    assert_ne!(silent, unrecognized);
}

#[test]
fn errors_wrap_external_causes() {
    let error = Error::from(Box::<dyn std::error::Error + Send + Sync>::from(
        "disk gone",
    ));
    assert_eq!(error.to_string(), "disk gone");
    assert_eq!(Error::Crashed.to_string(), "the simulated machine crashed");
}

fn bump(count: &Arc<AtomicUsize>) -> impl FnOnce() + Send + 'static {
    let count = Arc::clone(count);
    move || {
        count.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn on_cancel_fires_once() {
    let token = CancellationToken::new();
    let count = Arc::default();
    let guard = token.on_cancel(bump(&count));
    assert_eq!(count.load(Ordering::SeqCst), 0);
    token.cancel();
    token.cancel();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(guard);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn on_cancel_fires_immediately_when_already_cancelled() {
    let token = CancellationToken::new();
    token.cancel();
    let count = Arc::default();
    let _guard = token.on_cancel(bump(&count));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn ancestor_cancellation_fires_the_childs_callback() {
    let parent = CancellationToken::new();
    let child = parent.child_token();
    let count = Arc::default();
    let _guard = child.on_cancel(bump(&count));
    drop(child);
    parent.cancel();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn dropped_guard_never_fires() {
    let token = CancellationToken::new();
    let count = Arc::default();
    drop(token.on_cancel(bump(&count)));
    token.cancel();
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn callbacks_may_register_and_cancel_without_deadlock() {
    let token = CancellationToken::new();
    let count = Arc::default();
    let inner = token.clone();
    let nested = Arc::new(std::sync::Mutex::new(None));
    let stash = Arc::clone(&nested);
    let late = bump(&count);
    let _guard = token.on_cancel(move || {
        inner.cancel();
        *stash.lock().unwrap() = Some(inner.on_cancel(late));
    });
    token.cancel();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(nested.lock().unwrap().is_some());
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[test]
fn dropping_a_guard_whose_unrun_callback_cancels_the_token_does_not_deadlock() {
    let token = CancellationToken::new();
    let count = Arc::default();
    let capture = CancelOnDrop(token.clone());
    let run = bump(&count);
    let guard = token.on_cancel(move || {
        let _keep = &capture;
        run();
    });
    std::thread::spawn(move || drop(guard)).join().unwrap();
    assert!(token.is_cancelled());
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn dropping_a_guard_waits_for_a_callback_a_concurrent_cancel_already_claimed() {
    let token = CancellationToken::new();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let finished = Arc::new(AtomicBool::new(false));
    let work_done = Arc::clone(&finished);
    let guard = token.on_cancel(move || {
        started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        work_done.store(true, Ordering::SeqCst);
    });
    let canceller = std::thread::spawn(move || token.cancel());
    started_rx.recv().unwrap();
    let observed_done = Arc::clone(&finished);
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    let dropper = std::thread::spawn(move || {
        crate::cancellation::BEFORE_WAIT.with(|hook| *hook.borrow_mut() = Some(waiting_tx));
        drop(guard);
        observed_done.load(Ordering::SeqCst)
    });
    waiting_rx.recv().unwrap();
    assert!(!finished.load(Ordering::SeqCst));
    release_tx.send(()).unwrap();
    assert!(
        dropper.join().unwrap(),
        "the guard dropped while its callback was still running"
    );
    canceller.join().unwrap();
}

struct PanicsOnDrop;

impl Drop for PanicsOnDrop {
    fn drop(&mut self) {
        panic!("payload drop");
    }
}

#[test]
fn later_panic_payloads_are_never_dropped_during_dispatch() {
    let token = CancellationToken::new();
    let child = token.child_token();
    let count = Arc::default();
    let _first = token.on_cancel(|| panic!("first"));
    let _second = token.on_cancel(|| std::panic::panic_any(PanicsOnDrop));
    let _third = token.on_cancel(bump(&count));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| token.cancel()));
    assert!(outcome.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(child.is_cancelled());
}

#[test]
fn a_panicking_child_callback_resurfaces_without_sparing_siblings() {
    let parent = CancellationToken::new();
    let first = parent.child_token();
    let second = parent.child_token();
    let count = Arc::default();
    let _boom = first.on_cancel(|| panic!("child failed"));
    let _counted = second.on_cancel(bump(&count));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parent.cancel()));
    assert!(outcome.is_err());
    assert!(first.is_cancelled());
    assert!(second.is_cancelled());
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn a_callback_may_drop_its_own_guard() {
    let token = CancellationToken::new();
    let stash = Arc::new(std::sync::Mutex::new(None));
    let inside = Arc::clone(&stash);
    let ran = Arc::new(AtomicBool::new(false));
    let marked = Arc::clone(&ran);
    *stash.lock().unwrap() = Some(token.on_cancel(move || {
        drop(inside.lock().unwrap().take());
        marked.store(true, Ordering::SeqCst);
    }));
    token.cancel();
    assert!(ran.load(Ordering::SeqCst));
    assert!(stash.lock().unwrap().is_none());
}

#[test]
fn a_panicking_callback_does_not_stop_later_callbacks_or_children() {
    let token = CancellationToken::new();
    let child = token.child_token();
    let count = Arc::default();
    let _first = token.on_cancel(|| panic!("callback failed"));
    let _second = token.on_cancel(bump(&count));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| token.cancel()));
    assert!(outcome.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(child.is_cancelled());
}
