use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use test_case::test_case;
use togi_machine::Kernel as _;

use super::support::{
    LATER, OLD, boot, caused_by, kernel, locked, ok, pstore, scripted, scripted_at,
};
use crate::boot::JournalBoot;
use crate::runner::{JOURNALCTL_BUDGET, RunError};
use crate::{Error, Kernel};

fn timed_out() -> Result<togi_command::Output, RunError> {
    Err(RunError::Command(togi_command::Error::TimedOut {
        program: "journalctl".into(),
        timeout: JOURNALCTL_BUDGET,
    }))
}

#[test_case(&|k| drop(k.mces(&boot(OLD), Duration::ZERO)); "mces")]
#[test_case(&|k| drop(k.read_mces(&boot(OLD), None)); "read_mces")]
#[test_case(&|k| drop(k.reset_reason(&boot(OLD))); "reset_reason")]
#[test_case(&|k| drop(k.reset_reason_after(&boot(OLD))); "reset_reason_after")]
#[test_case(&|k| drop(k.saved_pstore(&boot(OLD))); "saved_pstore")]
fn every_seam_call_starts_its_command_with_the_whole_budget(call: &dyn Fn(&mut Kernel)) {
    let (mut kernel, recorded) = scripted(
        &[],
        pstore(&[("150/001/dmesg.txt", b"")]),
        JOURNALCTL_BUDGET,
        |_| ok(b"[]"),
    );
    call(&mut kernel);
    let recorded = locked(&recorded);
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].remaining <= JOURNALCTL_BUDGET);
    assert!(recorded[0].remaining + Duration::from_secs(5) > JOURNALCTL_BUDGET);
}

#[test]
fn a_command_that_times_out_leaves_no_observation() {
    let (mut kernel, _) = kernel(&[], |_| timed_out());
    let error = kernel.mces(&boot(OLD), Duration::ZERO).unwrap_err();
    assert!(
        caused_by::<togi_command::Error>(&error)
            .is_some_and(|source| matches!(source, togi_command::Error::TimedOut { .. }))
    );

    let partial = kernel.read_mces(&boot(OLD), None).unwrap_err();
    assert!(partial.value.is_empty());
    assert!(caused_by::<togi_command::Error>(&partial).is_some());

    assert!(kernel.reset_reason(&boot(OLD)).is_err());
    let error = kernel.reset_reason_after(&boot(OLD)).unwrap_err();
    assert!(caused_by::<togi_command::Error>(&error).is_some());
}

#[test]
fn the_commands_of_one_call_share_one_budget() {
    let list = json!([{"boot_id": OLD}, {"boot_id": LATER}]).to_string();
    let clock = Arc::new(Mutex::new(Instant::now()));
    let (kernel, calls) = scripted_at(
        &[],
        pstore(&[]),
        Duration::from_millis(150),
        Arc::clone(&clock),
        move |_| {
            *locked(&clock) += Duration::from_millis(250);
            ok(&list)
        },
    );
    let error = kernel
        .read_reset_reason_after(&JournalBoot::parse(OLD).unwrap())
        .unwrap_err();
    assert!(matches!(error, Error::BudgetExhausted { .. }), "{error:?}");
    assert_eq!(locked(&calls).len(), 1);
}

#[test]
fn later_commands_get_what_is_left_of_the_budget() {
    let list = json!([{"boot_id": OLD}, {"boot_id": LATER}]).to_string();
    let budget = Duration::from_secs(5);
    let clock = Arc::new(Mutex::new(Instant::now()));
    let (kernel, calls) = scripted_at(&[], pstore(&[]), budget, Arc::clone(&clock), move |args| {
        if args[0] == "--list-boots" {
            *locked(&clock) += Duration::from_millis(100);
            ok(&list)
        } else {
            ok("")
        }
    });
    let _ = kernel.read_reset_reason_after(&JournalBoot::parse(OLD).unwrap());
    let calls = locked(&calls);
    assert!(calls.len() >= 2);
    assert!(calls[1].remaining + Duration::from_millis(100) <= calls[0].remaining);
    assert!(!calls[1].remaining.is_zero());
}

#[test]
fn a_used_up_budget_runs_nothing() {
    let (kernel, calls) = scripted(&[], pstore(&[]), Duration::ZERO, |_| ok(""));
    let error = kernel
        .read_whole_boot(&JournalBoot::parse(OLD).unwrap(), Duration::ZERO)
        .unwrap_err();
    let togi_machine::Error::Other(source) = error.error else {
        panic!("{error:?}");
    };
    assert!(matches!(
        source.downcast_ref::<Error>(),
        Some(Error::BudgetExhausted { .. })
    ));
    assert!(locked(&calls).is_empty());
}

#[test]
fn every_seam_call_that_times_out_keeps_the_timeout_as_its_cause() {
    let timed_out_cause = |error: &(dyn std::error::Error + 'static)| {
        caused_by::<togi_command::Error>(error)
            .is_some_and(|source| matches!(source, togi_command::Error::TimedOut { .. }))
    };
    let (mut kernel, _) = kernel(&[], |_| timed_out());
    let partial = kernel.read_mces(&boot(OLD), None).unwrap_err();
    assert!(partial.value.is_empty());
    assert!(timed_out_cause(&partial), "{partial:?}");
    let error = kernel.reset_reason(&boot(OLD)).unwrap_err();
    assert!(timed_out_cause(&error), "{error:?}");
    let error = kernel.reset_reason_after(&boot(OLD)).unwrap_err();
    assert!(timed_out_cause(&error), "{error:?}");
}
