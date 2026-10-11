use crate::*;
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
    let waiter = {
        let descendant = descendant.clone();
        std::thread::spawn(move || {
            started.send(()).unwrap();
            descendant.wait();
            descendant.is_cancelled()
        })
    };
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
        signal: None,
        core: CoreId::new(0).unwrap(),
        inconclusive: String::new(),
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

    fn wait(&mut self, _cancel: &CancellationToken, _report: &mut dyn Reporter) -> Waited {
        Waited {
            result: result(Duration::from_secs(85)),
            error: Some(Error::Cancelled),
        }
    }

    fn stop(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

struct NoReporter;

impl Reporter for NoReporter {
    fn progress(&mut self, _detail: &str) {}
    fn sample(&mut self, _sample: Sample) {}
    fn signal(&mut self, _core: CoreId, _signal: Signal, _detail: &str) {}
}

#[test]
fn running_wait_returns_the_accumulated_result_with_its_error() {
    let mut running: Box<dyn Running> = Box::new(FailingRunning);
    let waited = running.wait(&CancellationToken::new(), &mut NoReporter);
    assert_eq!(waited.result, result(Duration::from_secs(85)));
    assert!(matches!(waited.error, Some(Error::Cancelled)));
}

struct PartialKernel;

impl Kernel for PartialKernel {
    fn mces(&mut self, _boot: &str, _since: Duration) -> Result<Vec<Mce>, Error> {
        Err(Error::BootMissing)
    }

    fn read_mces(&mut self, _boot: &str, cursor: &str) -> KernelRead {
        KernelRead {
            mces: Vec::new(),
            cursor: format!("{cursor}+1"),
            error: Some(Error::CursorMissing),
        }
    }

    fn reset_reason(&mut self, _boot: &str) -> Result<ResetReason, Error> {
        Err(Error::BootMissing)
    }

    fn reset_reason_after(&mut self, _boot: &str) -> Result<ResetReason, Error> {
        Err(Error::BootMissing)
    }

    fn saved_pstore(&mut self, _boot: &str) -> Result<Option<PstoreRecord>, Error> {
        Ok(None)
    }
}

#[test]
fn kernel_read_keeps_partial_evidence_with_its_error() {
    let mut kernel: Box<dyn Kernel> = Box::new(PartialKernel);
    let read = kernel.read_mces("boot", "c0");
    assert_eq!(read.cursor, "c0+1");
    assert!(matches!(read.error, Some(Error::CursorMissing)));
    assert!(matches!(
        kernel.mces("boot", Duration::ZERO),
        Err(Error::BootMissing)
    ));
}

#[test]
fn ranking_values_survive_a_failed_read() {
    let ranking = Ranking {
        ranks: vec![CoreRank {
            core: CoreId::new(1).unwrap(),
            value: 7,
        }],
        error: Some(Error::Crashed),
    };
    assert_eq!(ranking.ranks[0].value, 7);
    assert!(matches!(ranking.error, Some(Error::Crashed)));
}

#[test]
fn errors_wrap_external_causes() {
    let error = Error::from(Box::<dyn std::error::Error + Send + Sync>::from(
        "disk gone",
    ));
    assert_eq!(error.to_string(), "disk gone");
    assert_eq!(Error::Crashed.to_string(), "the simulated machine crashed");
}
