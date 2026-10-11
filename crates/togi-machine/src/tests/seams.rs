use crate::seams::SeamOutcome;
use crate::*;
use std::error::Error as StdError;
use std::fmt;

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

fn result(ran: i64) -> TrialResult {
    TrialResult {
        ran: NanoDuration(ran),
        signal: Signal::Empty,
        core: CoreId::new(0).unwrap(),
        inconclusive: String::new(),
        escaped: None,
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
    ) -> SeamOutcome<TrialResult> {
        SeamOutcome::failed(result(85), Error::Kind(ErrorKind::Cancelled))
    }

    fn stop(&mut self) -> SeamResult<()> {
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
    let outcome = running.wait(&CancellationToken::new(), &mut NoReporter);
    assert_eq!(outcome.value, result(85));
    assert!(
        outcome
            .error
            .as_ref()
            .unwrap()
            .contains(ErrorKind::Cancelled)
    );
}

struct PartialKernel;

impl Kernel for PartialKernel {
    fn mces(&mut self, _boot: &str, _since: NanoDuration) -> SeamOutcome<Vec<Mce>> {
        SeamOutcome::failed(Vec::new(), Error::Kind(ErrorKind::BootMissing))
    }

    fn read_mces(&mut self, _boot: &str, cursor: &str) -> SeamOutcome<KernelRead> {
        SeamOutcome::failed(
            KernelRead {
                mces: Some(Vec::new()),
                cursor: format!("{cursor}+1"),
            },
            Error::Context {
                detail: "decode kernel message".to_owned(),
                source: Box::new(Error::Kind(ErrorKind::CursorMissing)),
            },
        )
    }

    fn reset_reason(&mut self, _boot: &str) -> SeamResult<ResetReason> {
        Err(Error::Kind(ErrorKind::BootMissing))
    }

    fn reset_reason_after(&mut self, _boot: &str) -> SeamResult<ResetReason> {
        Err(Error::Kind(ErrorKind::BootMissing))
    }

    fn saved_pstore(&mut self, _boot: &str) -> SeamResult<Option<PstoreRecord>> {
        Ok(None)
    }
}

#[test]
fn kernel_read_keeps_partial_evidence_with_its_error() {
    let mut kernel: Box<dyn Kernel> = Box::new(PartialKernel);
    let read = kernel.read_mces("boot", "c0");
    assert_eq!(read.value.cursor, "c0+1");
    assert_eq!(read.value.mces, Some(Vec::new()));
    assert!(
        read.error
            .as_ref()
            .unwrap()
            .contains(ErrorKind::CursorMissing)
    );
    let legacy = kernel.mces("boot", NanoDuration(0));
    assert!(legacy.value.is_empty());
    assert!(
        legacy
            .error
            .as_ref()
            .unwrap()
            .contains(ErrorKind::BootMissing)
    );
}

struct RankingHost;

impl Host for RankingHost {
    fn boot_id(&mut self) -> SeamResult<String> {
        Ok(String::new())
    }

    fn topology(&mut self) -> SeamResult<Vec<CoreInfo>> {
        Ok(Vec::new())
    }

    fn validate_smu(&mut self) -> SeamResult<()> {
        Ok(())
    }

    fn bios_context(&mut self) -> SeamResult<BiosContext> {
        Ok(BiosContext::default())
    }

    fn ranking(&mut self) -> SeamOutcome<Option<Vec<CoreRank>>> {
        SeamOutcome::failed(
            Some(vec![CoreRank {
                core: CoreId::new(1).unwrap(),
                value: 7,
            }]),
            Error::Kind(ErrorKind::Crashed),
        )
    }

    fn preflight(&mut self) -> Vec<Check> {
        Vec::new()
    }

    fn watchdog(&mut self) -> Check {
        Check::default()
    }
}

#[test]
fn ranking_values_survive_a_failed_read() {
    let outcome = RankingHost.ranking();
    assert_eq!(outcome.value.unwrap()[0].value, 7);
    assert!(outcome.error.unwrap().contains(ErrorKind::Crashed));
}

#[test]
fn outcome_into_result_keeps_the_value_only_without_an_error() {
    assert_eq!(SeamOutcome::ok(3).into_result().unwrap(), 3);
    let failed = SeamOutcome::failed(3, Error::Kind(ErrorKind::Crashed)).into_result();
    assert!(failed.unwrap_err().contains(ErrorKind::Crashed));
}

#[derive(Debug)]
struct ExternalError {
    source: ErrorKind,
}

impl fmt::Display for ExternalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("external failure")
    }
}

impl StdError for ExternalError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
struct Layered(Box<dyn StdError + Send + Sync>);

impl fmt::Display for Layered {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("layered failure")
    }
}

impl StdError for Layered {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.0.as_ref())
    }
}

#[test]
fn bare_sentinel_matches_only_its_kind() {
    let error = Error::Kind(ErrorKind::Containment);
    assert!(error.contains(ErrorKind::Containment));
    assert!(!error.contains(ErrorKind::Crashed));
    let boxed = Error::Other(Box::new(ErrorKind::Containment));
    assert!(boxed.contains(ErrorKind::Containment));
    assert!(!boxed.contains(ErrorKind::Crashed));
}

#[test]
fn sentinel_is_found_through_an_external_source_chain() {
    let error = Error::Other(Box::new(ExternalError {
        source: ErrorKind::Containment,
    }));
    assert!(error.contains(ErrorKind::Containment));
    assert!(!error.contains(ErrorKind::Crashed));
    let deep = Error::Other(Box::new(Layered(Box::new(ExternalError {
        source: ErrorKind::Containment,
    }))));
    assert!(deep.contains(ErrorKind::Containment));
    assert!(!deep.contains(ErrorKind::BootMissing));
    let context = Error::Context {
        detail: "teardown".to_owned(),
        source: Box::new(deep),
    };
    assert!(context.contains(ErrorKind::Containment));
}

#[test]
fn external_error_without_a_sentinel_matches_nothing() {
    let error = Error::Other(Box::new(std::io::Error::other("disk gone")));
    assert!(!error.contains(ErrorKind::Containment));
}
