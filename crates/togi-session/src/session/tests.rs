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

#[test]
fn a_configured_core_the_machine_lacks_is_refused() {
    let mut bench = Bench::new(2);
    bench.config = togi_config::Config::parse(r#"{"start_offsets":{"5":-5}}"#).unwrap();
    let mut session = bench.open();
    assert!(matches!(
        session.start(),
        Err(SessionError::NoSuchCore { core }) if core == crate::testing::core(5)
    ));
    assert!(crate::testing::kinds_of(session.appender()).is_empty());
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
    let mut session = bench.open();
    let inner = std::mem::replace(&mut session.host, bench.sim.seams().host);
    session.host = Box::new(ReadFails {
        inner,
        topology: Some(unreadable),
        context: None,
    });
    assert!(matches!(session.start(), Err(SessionError::Machine(_))));
    assert!(crate::testing::kinds_of(session.appender()).is_empty());
    untouched(&bench, &session);
    drop(session);

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
