use test_case::test_case;
use togi_journal::{CrashDetected, SmuError, SmuOp};

use super::*;
use crate::testing::Bench;

#[test]
fn start_records_the_session_and_its_configuration() {
    let bench = Bench::new(2);
    let mut session = bench.open();
    assert_eq!(session.start().unwrap(), Control::Continue);
    assert_eq!(
        crate::testing::kinds_of(session.appender()),
        ["session.start", "config.loaded"]
    );
    assert_eq!(
        session.cores().iter().collect::<Vec<_>>(),
        [crate::testing::core(0), crate::testing::core(1)]
    );
    crate::testing::assert_fold_consistent(session.appender());
}

#[test]
fn a_second_run_on_the_boot_keeps_the_session_start() {
    let bench = Bench::new(2);
    let mut first = bench.open();
    first.start().unwrap();
    drop(first);
    let mut second = bench.open();
    assert!(second.same_boot());
    second.start().unwrap();
    assert_eq!(
        crate::testing::kinds_of(second.appender()),
        ["session.start", "config.loaded", "config.loaded"]
    );
}

#[test]
fn a_missing_watchdog_is_warned_about_outside_the_tuning_boot() {
    let bench = Bench::new(2);
    bench.sim.fail_check("watchdog", "no /dev/watchdog");
    let mut session = bench.open();
    session.start().unwrap();
    let last = session.appender().journal().events().last().unwrap();
    let Payload::SessionWarning(warning) = &last.payload else {
        panic!("{:?}", last.payload);
    };
    assert_eq!(warning.operation, "check hardware watchdog");
    assert!(
        warning
            .error
            .starts_with("no active hardware watchdog (no /dev/watchdog)")
    );
}

fn smu_offsets(bench: &Bench) -> Vec<i64> {
    let mut smu = bench.sim.seams().smu;
    (0..2)
        .map(|n| smu.offset(crate::testing::core(n)).unwrap().0)
        .collect()
}

#[test_case("start_offsets"; "start_offsets")]
#[test_case("candidate_solo_limits"; "candidate_solo_limits")]
fn a_configured_core_the_machine_lacks_is_refused(source: &str) {
    let mut bench = Bench::new(2);
    bench.config = togi_config::Config::parse(&format!(r#"{{"{source}":{{"5":-10}}}}"#)).unwrap();
    let before = smu_offsets(&bench);
    let ended = crate::run::run(bench.input());
    assert!(
        matches!(
            ended.errors.as_slice(),
            [SessionError::NoSuchCore { core }] if *core == crate::testing::core(5)
        ),
        "{source}: {:?}",
        ended.errors
    );
    assert_eq!(ended.stop, None, "{source}");
    assert!(ended.journal.events().is_empty(), "{source}");
    assert_eq!(smu_offsets(&bench), before, "{source}");
}

#[test]
fn a_journal_of_another_build_is_refused_before_any_append() {
    let bench = Bench::new(2);
    let mut session = bench.open();
    session.start().unwrap();
    drop(session);
    let mut input = bench.input();
    input.build.ruleset += 1;
    let Err(ended) = Session::open(input) else {
        panic!("opened");
    };
    assert!(matches!(
        ended.errors.as_slice(),
        [SessionError::Compatibility(_)]
    ));
    assert_eq!(ended.journal.events().len(), 2);
}

#[test]
fn no_evidence_is_no_dead_end() {
    let bench = Bench::new(2);
    let mut session = bench.open();
    session.start().unwrap();
    assert_eq!(session.check_dead_end().unwrap(), None);
    assert_eq!(session.shutdown, None);
}

fn stray_crash(session: &mut Session) -> Seq {
    let crash = CrashDetected {
        previous_boot: session.boot.clone(),
        in_flight: None,
        stray: true,
        condition: None,
        reset_reason: None,
        reset_reason_raw: Vec::new(),
        inconclusive: false,
        pstore: None,
    };
    session
        .appender
        .append(Payload::CrashDetected(crash), &[])
        .unwrap()
}

#[test]
fn stray_crashes_end_in_a_boot_loop_that_exits_without_a_bootloader() {
    let bench = Bench::new(2);
    let mut session = bench.open();
    session.start().unwrap();
    let crashes: Vec<Seq> = (0..3).map(|_| stray_crash(&mut session)).collect();
    let stop = session.check_dead_end().unwrap();
    let Some(Stop::DeadEnd {
        dead_end,
        causes,
        reboot: false,
    }) = stop
    else {
        panic!("{stop:?}");
    };
    assert_eq!(dead_end.condition, DeadEndCondition::BootLoop);
    assert_eq!(
        dead_end.detail,
        "3 crashes in a row before the profile was applied"
    );
    assert_eq!(dead_end.action, Some(DeadEndAction::Exit));
    assert_eq!(causes, crashes);
    assert_eq!(
        session.shutdown.as_ref().map(|shutdown| shutdown.reason),
        Some(ShutdownReason::DeadEnd)
    );
    let last = session.appender().journal().events().last().unwrap();
    assert_eq!(last.cause, crashes);
    crate::testing::assert_fold_consistent(session.appender());
}

#[test]
fn smu_evidence_outranks_a_boot_loop() {
    let bench = Bench::new(2);
    let mut session = bench.open();
    session.start().unwrap();
    for _ in 0..3 {
        stray_crash(&mut session);
    }
    let error = SmuError {
        op: SmuOp::Read,
        core: Some(crate::testing::core(1)),
        offset: None,
        error: "mailbox timeout".into(),
    };
    let cited = session
        .appender
        .append(Payload::SmuError(error), &[])
        .unwrap();
    let stop = session.check_dead_end().unwrap().unwrap();
    let Stop::DeadEnd {
        dead_end, causes, ..
    } = stop
    else {
        panic!("{stop:?}");
    };
    assert_eq!(dead_end.condition, DeadEndCondition::Smu);
    assert_eq!(causes, [cited]);
}

#[test]
fn a_signal_stop_records_its_shutdown_for_the_cleanup() {
    let bench = Bench::new(2);
    let mut session = bench.open();
    session.start().unwrap();
    assert_eq!(session.shut_down(Stop::Signal), Control::Stop(Stop::Signal));
    assert_eq!(
        session.shutdown.as_ref().map(|shutdown| shutdown.reason),
        Some(ShutdownReason::Signal)
    );
}

#[test]
fn a_dead_end_pending_from_another_boot_is_finished_first() {
    let bench = Bench::new(2);
    let mut session = bench.open();
    session.start().unwrap();
    let dead_end = DeadEnd {
        condition: DeadEndCondition::Smu,
        core: None,
        detail: "smu gone".into(),
        action: Some(DeadEndAction::ClearSavedEntry),
    };
    session
        .appender
        .append(Payload::DeadEnd(dead_end.clone()), &[])
        .unwrap();
    drop(session);
    bench.sim.reboot();
    let mut session = bench.open();
    let Control::Stop(stop) = session.start().unwrap() else {
        panic!("continued");
    };
    let Stop::DeadEnd {
        dead_end: recorded,
        reboot,
        ..
    } = stop
    else {
        panic!("{stop:?}");
    };
    assert_eq!(recorded, dead_end);
    assert!(!reboot);
    assert_eq!(
        crate::testing::kinds_of(session.appender()),
        ["session.start", "config.loaded", "deadend"]
    );
}

fn unreadable() -> togi_machine::Error {
    togi_machine::Error::Other("hardware identity unavailable".into())
}

fn untouched(bench: &Bench, session: &Session) {
    let mut smu = bench.sim.seams().smu;
    for core in session.cores() {
        assert_eq!(smu.offset(core).unwrap().0, 0);
    }
    let kinds = crate::testing::kinds_of(session.appender());
    for kind in ["smu.intent", "trial.intent", "shutdown"] {
        assert!(!kinds.contains(&kind), "{kind}");
    }
}

#[test]
fn a_host_that_cannot_be_read_does_not_start_tuning() {
    use crate::testing::ReadFails;

    let bench = Bench::new(2);
    let unreadable_topology = |bench: &Bench| {
        let mut input = bench.input();
        let inner = input.machine.host;
        input.machine.host = Box::new(ReadFails {
            inner,
            topology: Some(unreadable),
            context: None,
        });
        input
    };
    let refused = |bench: &Bench, when: &str| {
        let Err(ended) = Session::open(unreadable_topology(bench)) else {
            panic!("{when}: opened without the topology");
        };
        assert!(
            matches!(ended.errors.as_slice(), [SessionError::Machine(_)]),
            "{when}: {:?}",
            ended.errors
        );
        ended.journal.events().len()
    };
    assert_eq!(refused(&bench, "new session"), 0);
    bench.open().start().unwrap();
    assert_eq!(refused(&bench, "resumed session"), 2);

    let mut session = crate::testing::swept(&bench);
    let inner = std::mem::replace(&mut session.host, bench.sim.seams().host);
    session.host = Box::new(ReadFails {
        inner,
        topology: None,
        context: Some(unreadable),
    });
    assert!(matches!(session.begin(), Err(SessionError::Machine(_))));
    let kinds = crate::testing::kinds_of(session.appender());
    assert!(!kinds.contains(&"session.baseline") && !kinds.contains(&"core.phase"));
    untouched(&bench, &session);
}

#[test]
fn an_identity_that_cannot_be_allocated_leaves_the_journal_empty() {
    use togi_journal::fs::{Fault, Op};

    let bench = Bench::new(2);
    let mut session = bench.open();
    bench.mem.inject(Fault::before(Op::Read, 1));
    assert!(session.start().is_err());
    assert!(crate::testing::kinds_of(session.appender()).is_empty());
    untouched(&bench, &session);
}

#[test]
fn a_refused_journal_whose_saved_entry_cannot_be_cleared_keeps_both_errors() {
    let bench = Bench::new(2);
    let mut session = bench.open();
    session.start().unwrap();
    drop(session);
    let mut input = bench.input();
    input.build.ruleset += 1;
    input.bootloader = Some(Box::new(crate::testing::ClearFails::default()));
    let Err(ended) = Session::open(input) else {
        panic!("opened");
    };
    assert!(matches!(
        ended.errors.as_slice(),
        [
            SessionError::Compatibility(_),
            SessionError::ClearSavedEntry(_)
        ]
    ));
    assert_eq!(ended.journal.events().len(), 2);
}

type Reshape = fn(Vec<togi_machine::CoreInfo>) -> Vec<togi_machine::CoreInfo>;

struct Cores(Box<dyn togi_machine::Host>, Reshape);

impl togi_machine::Host for Cores {
    fn boot_id(&mut self) -> Result<togi_machine::BootId, togi_machine::Error> {
        self.0.boot_id()
    }

    fn topology(&mut self) -> Result<Vec<togi_machine::CoreInfo>, togi_machine::Error> {
        self.0.topology().map(self.1)
    }

    fn validate_smu(&mut self) -> Result<togi_machine::SmuValidated, togi_machine::Error> {
        self.0.validate_smu()
    }

    fn bios_context(&mut self) -> Result<togi_machine::BiosContext, togi_machine::Error> {
        self.0.bios_context()
    }

    fn ranking(
        &mut self,
    ) -> Result<Vec<togi_machine::CoreRank>, togi_machine::Partial<Vec<togi_machine::CoreRank>>>
    {
        self.0.ranking()
    }

    fn preflight(&mut self) -> Vec<togi_machine::Check> {
        self.0.preflight()
    }

    fn watchdog(&mut self) -> togi_machine::Check {
        self.0.watchdog()
    }
}

fn without_core_one(mut cores: Vec<togi_machine::CoreInfo>) -> Vec<togi_machine::CoreInfo> {
    cores.retain(|info| info.core != crate::testing::core(1));
    cores
}

fn with_core_five(mut cores: Vec<togi_machine::CoreInfo>) -> Vec<togi_machine::CoreInfo> {
    let mut added = cores[0].clone();
    added.core = crate::testing::core(5);
    cores.push(added);
    cores
}

fn in_four_ccds(mut cores: Vec<togi_machine::CoreInfo>) -> Vec<togi_machine::CoreInfo> {
    for info in &mut cores {
        info.ccd = togi_machine::CcdId::try_from(info.core.get() / 2).unwrap();
    }
    cores
}

fn on_four_ccds(bench: &Bench) -> RunInput<'static> {
    let mut input = bench.input();
    let host = input.machine.host;
    input.machine.host = Box::new(Cores(host, in_four_ccds));
    input
}

#[test]
fn a_topology_the_tuner_cannot_work_on_is_refused_before_any_append_on_every_run() {
    let bench = Bench::new(8);
    let before = smu_offsets(&bench);
    for run in 0..2 {
        let ended = crate::run::run(on_four_ccds(&bench));
        assert!(
            matches!(
                ended.errors.as_slice(),
                [SessionError::UnsupportedTopology(
                    togi_tuner::TopologyError::TooManyCcds(4)
                )]
            ),
            "run {run}: {:?}",
            ended.errors
        );
        assert_eq!(ended.stop, None, "run {run}");
        assert!(ended.journal.events().is_empty(), "run {run}");
    }
    assert_eq!(smu_offsets(&bench), before);
    let mut session = bench.open();
    session.start().unwrap();
    assert_eq!(
        crate::testing::kinds_of(session.appender()),
        ["session.start", "config.loaded"]
    );
}

#[test]
fn a_refused_topology_leaves_the_tuning_boot_and_keeps_a_failed_clear() {
    let bench = Bench::new(8);
    let mut input = on_four_ccds(&bench);
    input.bootloader = Some(Box::new(crate::testing::ClearFails::default()));
    let Err(ended) = Session::open(input) else {
        panic!("opened");
    };
    assert!(
        matches!(
            ended.errors.as_slice(),
            [
                SessionError::UnsupportedTopology(_),
                SessionError::ClearSavedEntry(_)
            ]
        ),
        "{:?}",
        ended.errors
    );
    assert!(ended.journal.events().is_empty());
}

#[test_case(without_core_one, &[], &[1]; "removed core")]
#[test_case(with_core_five, &[5], &[]; "added core")]
fn a_resumed_session_refuses_added_or_removed_cores_before_any_append(
    change: Reshape,
    added: &[u8],
    removed: &[u8],
) {
    let bench = Bench::new(2);
    let mut first = bench.open();
    first.start().unwrap();
    drop(first);
    let mut input = bench.input();
    let host = input.machine.host;
    input.machine.host = Box::new(Cores(host, change));
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    let before = crate::testing::kinds_of(session.appender());
    let Err(SessionError::TopologyChanged {
        added: got_added,
        removed: got_removed,
    }) = session.start()
    else {
        panic!("not refused");
    };
    let ids = |set: togi_machine::CoreSet| {
        set.into_iter()
            .map(togi_machine::CoreId::get)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(got_added), added);
    assert_eq!(ids(got_removed), removed);
    assert_eq!(crate::testing::kinds_of(session.appender()), before);
}

#[test]
fn a_changed_topology_is_refused_before_finishing_a_prior_boots_dead_end() {
    let bench = Bench::new(2);
    let mut first = bench.open();
    first.start().unwrap();
    first
        .appender
        .append(
            Payload::DeadEnd(DeadEnd {
                condition: DeadEndCondition::Smu,
                core: None,
                detail: "smu gone".into(),
                action: Some(DeadEndAction::ClearSavedEntry),
            }),
            &[],
        )
        .unwrap();
    drop(first);
    bench.sim.reboot();
    let mut input = bench.input();
    let host = input.machine.host;
    input.machine.host = Box::new(Cores(host, with_core_five));
    input.bootloader = Some(Box::new(crate::testing::MemBoot::default()));
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    assert!(!session.same_boot());
    let before = crate::testing::kinds_of(session.appender());
    let result = session.start();
    assert_eq!(crate::testing::kinds_of(session.appender()), before);
    assert!(matches!(result, Err(SessionError::TopologyChanged { .. })));
}

struct ReasonUnreadable;

impl togi_tuningboot::Environment for ReasonUnreadable {
    fn get(&mut self, _name: &str) -> io::Result<Option<String>> {
        Err(io::Error::other("GRUB environment unreadable"))
    }

    fn set(&mut self, _values: &[(&str, &str)]) -> io::Result<()> {
        Err(io::Error::other("the bootloader must not be written"))
    }

    fn unset(&mut self, _names: &[&str]) -> io::Result<()> {
        Err(io::Error::other("the bootloader must not be written"))
    }
}

impl Bootloader for ReasonUnreadable {
    fn clear_saved_entry(&mut self) -> io::Result<togi_tuningboot::SavedEntry> {
        Err(io::Error::other("the bootloader must not be written"))
    }
}

fn assert_returned_untouched(bench: &Bench, ended: &Ended, events: usize) {
    assert_eq!(ended.stop, None);
    assert!(ended.emergency.is_none());
    assert!(ended.capsule.is_none());
    assert_eq!(ended.journal.events().len(), events);
    assert_eq!(smu_offsets(bench), [0, 0]);
}

#[test]
fn a_tuning_boot_reason_that_cannot_be_read_returns_the_journal_untouched() {
    let bench = Bench::new(2);
    let mut first = bench.open();
    first.start().unwrap();
    let events = first.appender().journal().events().len();
    drop(first);
    let mut input = bench.input();
    input.bootloader = Some(Box::new(ReasonUnreadable));
    let ended = crate::run::run(input);
    assert!(
        matches!(ended.errors.as_slice(), [SessionError::Bootloader(_)]),
        "{:?}",
        ended.errors
    );
    assert_returned_untouched(&bench, &ended, events);
}

#[test]
fn a_compatible_journal_that_cannot_be_folded_is_returned_untouched() {
    let bench = Bench::new(2);
    let mut first = crate::testing::begun(&bench);
    let unfoldable = togi_journal::CoreTransition {
        core: crate::testing::core(9),
        from: None,
        to: togi_journal::CorePhase::AtLimit,
        offset: crate::testing::target(-6),
        pass: None,
        failure_point: None,
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".into(),
    };
    let error = first
        .appender
        .append(Payload::CorePhase(unfoldable), &[])
        .unwrap_err();
    assert!(matches!(error, crate::error::AppendFailure::Fold(_)));
    let events = first.appender().journal().events().len();
    drop(first);
    let mut input = bench.input();
    input.bootloader = Some(Box::new(crate::testing::MemBoot::default()));
    let ended = crate::run::run(input);
    assert!(
        matches!(ended.errors.as_slice(), [SessionError::Fold(_)]),
        "{:?}",
        ended.errors
    );
    assert_returned_untouched(&bench, &ended, events);
}

type Read = Result<togi_machine::KernelRead, Partial<Vec<togi_machine::Mce>>>;

type Seen = std::rc::Rc<std::cell::RefCell<Vec<Option<String>>>>;

/// A kernel whose log reads answer from a script, in order, and remember the cursor each was asked to resume after.
struct Scripted {
    inner: Box<dyn Kernel>,
    reads: std::collections::VecDeque<Read>,
    seen: Seen,
}

impl Kernel for Scripted {
    fn mces(
        &mut self,
        boot: &BootId,
        since: Duration,
    ) -> Result<Vec<togi_machine::Mce>, Partial<Vec<togi_machine::Mce>>> {
        self.inner.mces(boot, since)
    }

    fn read_mces(
        &mut self,
        _boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<togi_machine::KernelRead, Partial<Vec<togi_machine::Mce>>> {
        self.seen
            .borrow_mut()
            .push(after.map(|cursor| cursor.as_str().to_owned()));
        self.reads.pop_front().expect("the script ran out of reads")
    }

    fn reset_reason(&mut self, boot: &BootId) -> Result<togi_machine::ResetReason, Error> {
        self.inner.reset_reason(boot)
    }

    fn reset_reason_after(&mut self, boot: &BootId) -> Result<togi_machine::ResetReason, Error> {
        self.inner.reset_reason_after(boot)
    }

    fn saved_pstore(&mut self, boot: &BootId) -> Result<Option<togi_machine::PstoreRecord>, Error> {
        self.inner.saved_pstore(boot)
    }
}

fn machine_check(monotonic: Duration, bank: u16, label: &str) -> togi_machine::Mce {
    togi_machine::Mce {
        cpu: togi_machine::CpuId::from(0),
        core: Some(crate::testing::core(0)),
        bank,
        bank_type: togi_machine::BankType::LoadStore,
        corrected: true,
        time: jiff::Timestamp::UNIX_EPOCH,
        monotonic,
        lines: vec![label.to_owned()],
    }
}

fn partial(mces: Vec<togi_machine::Mce>, error: Error) -> Read {
    Err(Partial { value: mces, error })
}

/// A session that opens the bench's journal on the simulator's current boot over a kernel that answers `script`.
fn resumed_over(bench: &Bench, script: Vec<Read>) -> (Session<'static>, Seen) {
    let mut input = bench.input();
    let seen = Seen::default();
    let inner = input.machine.kernel;
    input.machine.kernel = Box::new(Scripted {
        inner,
        reads: script.into(),
        seen: seen.clone(),
    });
    let Ok(session) = Session::open(input) else {
        panic!("open");
    };
    (session, seen)
}

/// The same, after a first session recorded a kernel cursor, which is returned.
fn scripted(bench: &Bench, script: Vec<Read>) -> (Session<'static>, KernelCursor, Seen) {
    let mut first = bench.open();
    first.start().unwrap();
    let cursor = first
        .appender()
        .recovery()
        .kernel_cursor(&first.boot)
        .cloned()
        .expect("startup records the kernel cursor");
    drop(first);
    let (session, seen) = resumed_over(bench, script);
    (session, cursor, seen)
}

#[test]
fn a_crash_on_the_read_after_a_lost_cursor_ends_the_step_with_no_evidence_recorded() {
    let bench = Bench::new(2);
    let lost = machine_check(Duration::ZERO, 1, "read before the cursor was lost");
    let (mut session, cursor, seen) = scripted(
        &bench,
        vec![
            partial(vec![lost], Error::CursorMissing),
            partial(Vec::new(), Error::Crashed),
        ],
    );
    let before = crate::testing::kinds_of(session.appender());
    let Err(error) = session.start() else {
        panic!("the step went on after the machine crashed");
    };
    assert!(error.is_crash(), "{error:?}");
    assert_eq!(crate::testing::kinds_of(session.appender()), before);
    assert_eq!(*seen.borrow(), [Some(cursor.as_str().to_owned()), None]);
}

#[test]
fn the_machine_checks_of_both_partial_reads_survive_a_failed_reanchor() {
    let bench = Bench::new(2);
    let now = bench.sim.monotonic();
    let in_trial = machine_check(now, 1, "inside the window");
    let after_window = machine_check(now + Duration::from_secs(1), 2, "after the window");
    let (mut session, cursor, seen) = scripted(
        &bench,
        vec![
            partial(vec![in_trial.clone()], Error::CursorMissing),
            partial(
                vec![in_trial, after_window],
                Error::Other("reanchor failed".into()),
            ),
        ],
    );
    let trial = TrialId::from("20260101T000000Z-t1".to_owned());
    let window = TrialWindow {
        trial: &trial,
        since: Duration::ZERO,
    };
    let boundary = session.kernel_boundary(Some(window), false, &[]).unwrap();
    assert_eq!(
        boundary.error.as_deref(),
        Some("kernel log unreadable: kernel log cursor cannot be resumed; reanchor failed")
    );
    assert_eq!(boundary.cursor, Some(cursor.clone()));
    assert_eq!(*seen.borrow(), [Some(cursor.as_str().to_owned()), None]);
    let recorded: Vec<_> = session
        .appender()
        .journal()
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::Mce(mce) = &event.payload {
                Some((mce.lines.clone(), mce.trial.clone(), mce.between_trials))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        recorded,
        [
            (vec!["inside the window".to_owned()], Some(trial), false),
            (vec!["after the window".to_owned()], None, true),
        ]
    );
}

#[test_case(false; "false")]
#[test_case(true; "true")]
fn a_reopened_session_resumes_its_own_boots_cursor_and_files_the_tail_between_trials(reboot: bool) {
    use togi_machine::Regime;

    let bench = Bench::new(2);
    let mut first = crate::testing::begun(&bench);
    let plan = crate::testing::search_plan(&first, crate::testing::core(0), Regime::R1);
    assert_eq!(
        crate::testing::run_plan(&mut first, plan).unwrap(),
        Control::Continue
    );
    let recorded = first
        .appender()
        .recovery()
        .kernel_cursor(&first.boot)
        .cloned()
        .expect("the trial recorded the kernel cursor");
    drop(first);
    if reboot {
        bench.sim.reboot();
    }
    let tail = machine_check(bench.sim.monotonic(), 3, "after the last trial");
    let resumed = KernelCursor::from("resumed".to_owned());
    let (mut session, seen) = resumed_over(
        &bench,
        vec![Ok(togi_machine::KernelRead {
            mces: vec![tail],
            cursor: resumed.clone(),
        })],
    );
    assert_eq!(session.same_boot(), !reboot);
    assert_eq!(session.start().unwrap(), Control::Continue);
    let expected = (!reboot).then(|| recorded.as_str().to_owned());
    assert_eq!(*seen.borrow(), [expected], "reboot={reboot}");
    let events = session.appender().journal().events();
    let found: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::Mce(mce) = &event.payload {
                Some(mce)
            } else {
                None
            }
        })
        .collect();
    let [mce] = found.as_slice() else {
        panic!("reboot={reboot}: {found:?}");
    };
    assert!(
        mce.between_trials && mce.trial.is_none() && mce.from_boot.is_none(),
        "reboot={reboot}: {mce:?}"
    );
    let loaded = events
        .iter()
        .rev()
        .find_map(|event| {
            if let Payload::ConfigLoaded(loaded) = &event.payload {
                Some(loaded)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(loaded.kernel_cursor, Some(resumed), "reboot={reboot}");
}
