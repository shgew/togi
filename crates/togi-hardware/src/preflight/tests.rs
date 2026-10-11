use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;
use togi_machine::Check;
use togi_mprime::Mprime;
use togi_trial::{BackendIdentity, Backends, IdentityError};
use togi_ycruncher::Ycruncher;

use super::{Account, Outcome, STEPS, Step, check_backends, run, skip};
use crate::checks::{CheckName, Skip};

fn ok(name: CheckName) -> Check {
    Check {
        name: name.as_str().to_owned(),
        detail: String::new(),
        ok: true,
    }
}

fn outcomes(privileged: bool, failing: &[Step]) -> Vec<(CheckName, Option<Skip>, bool)> {
    run(privileged, |step| Check {
        ok: !failing.contains(&step),
        ..ok(step.name())
    })
    .into_iter()
    .map(|outcome| match outcome {
        Outcome::Ran(check) => (name_of(&check), None, check.ok),
        Outcome::Skipped(skipped) => (skipped.name, Some(skipped.reason), false),
    })
    .collect()
}

fn name_of(check: &Check) -> CheckName {
    STEPS
        .into_iter()
        .map(Step::name)
        .find(|name| name.as_str() == check.name)
        .unwrap()
}

#[test]
fn the_steps_run_in_the_order_of_the_runtime_spec() {
    let names: Vec<_> = STEPS.into_iter().map(|step| step.name().as_str()).collect();
    assert_eq!(
        names,
        [
            "cpu",
            "ryzen_smu",
            "readback",
            "pm_table",
            "slot_mapping",
            "backends",
            "backend_user",
            "systemd_run"
        ]
    );
}

#[test]
fn a_privileged_identified_run_skips_nothing() {
    assert!(
        STEPS
            .into_iter()
            .all(|step| skip(step, true, true).is_none())
    );
    assert!(
        outcomes(true, &[])
            .iter()
            .all(|(_, skipped, ok)| skipped.is_none() && *ok)
    );
}

#[test]
fn without_root_the_hardware_and_systemd_steps_are_skipped_for_root() {
    let skipped: Vec<_> = outcomes(false, &[])
        .into_iter()
        .filter(|(_, skipped, _)| *skipped == Some(Skip::NeedsRoot))
        .map(|(name, ..)| name)
        .collect();
    assert_eq!(
        skipped,
        [
            CheckName::Readback,
            CheckName::PmTable,
            CheckName::SlotMapping,
            CheckName::SystemdRun
        ]
    );
}

#[test]
fn the_ran_and_skipped_steps_of_an_unprivileged_run_cover_every_step_once() {
    let outcomes = outcomes(false, &[]);
    let names: Vec<_> = outcomes.iter().map(|(name, ..)| *name).collect();
    let expected: Vec<_> = STEPS.into_iter().map(Step::name).collect();
    assert_eq!(names, expected);
}

#[test]
fn a_failed_identity_check_skips_every_check_that_needs_the_identity() {
    for failing in [Step::Cpu, Step::RyzenSmu] {
        let outcomes = outcomes(true, &[failing]);
        for (name, skipped, _) in outcomes {
            let gated = !matches!(name, CheckName::Cpu | CheckName::RyzenSmu);
            let expected = gated.then_some(Skip::NeedsIdentity);
            let before_failure = failing == Step::RyzenSmu && name == CheckName::Cpu;
            assert_eq!(
                skipped,
                if before_failure { None } else { expected },
                "{name}"
            );
        }
    }
}

#[test]
fn both_identity_checks_run_even_after_the_first_fails() {
    let outcomes = outcomes(true, &[Step::Cpu]);
    assert_eq!(outcomes[0], (CheckName::Cpu, None, false));
    assert_eq!(outcomes[1], (CheckName::RyzenSmu, None, true));
}

#[test]
fn an_unprivileged_caller_is_told_it_needs_root_before_it_is_told_the_identity_failed() {
    assert_eq!(skip(Step::Readback, false, false), Some(Skip::NeedsRoot));
    assert_eq!(
        skip(Step::Backends, false, false),
        Some(Skip::NeedsIdentity)
    );
    assert_eq!(skip(Step::Backends, false, true), None);
}

#[test]
fn a_skipped_step_becomes_a_failed_check_carrying_its_reason() {
    let check = run(false, |step| ok(step.name()))
        .into_iter()
        .map(Outcome::into_check)
        .find(|check| check.name == "readback")
        .unwrap();
    assert!(!check.ok);
    assert_eq!(check.detail, "needs root");
}

fn install(root: &Path, relative: &str, mode: u32) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
}

fn package(executable: bool) -> TempDir {
    let root = TempDir::new().unwrap();
    let mode = if executable { 0o755 } else { 0o644 };
    for relative in [
        "bin/mprime",
        "lib/y-cruncher/Binaries/00-x64 ~ baseline",
        "lib/y-cruncher/Binaries/24-ZN5 ~ Zen 5",
    ] {
        install(root.path(), relative, mode);
    }
    root
}

fn backends(root: &Path, mprime: bool, ycruncher: bool) -> Backends {
    let mut backends = Backends::new();
    if mprime {
        backends = backends.with(Arc::new(Mprime::new(root)));
    }
    if ycruncher {
        backends = backends.with(Arc::new(Ycruncher::new(root)));
    }
    backends
}

#[test]
fn unconfigured_backends_fail_naming_both() {
    let root = package(true);
    let check = check_backends(&backends(root.path(), false, false));
    assert!(!check.ok);
    assert_eq!(check.name, "backends");
    assert_eq!(
        check.detail,
        "backends.mprime not configured; backends.ycruncher not configured"
    );
}

#[test]
fn a_missing_backend_fails_and_the_other_is_still_reported() {
    let root = package(true);
    let check = check_backends(&backends(root.path(), true, false));
    assert!(!check.ok);
    assert!(check.detail.starts_with("mprime: "), "{}", check.detail);
    assert!(
        check
            .detail
            .ends_with("; backends.ycruncher not configured"),
        "{}",
        check.detail
    );

    let check = check_backends(&backends(root.path(), false, true));
    assert!(!check.ok);
    assert!(
        check
            .detail
            .starts_with("backends.mprime not configured; ycruncher: "),
        "{}",
        check.detail
    );
}

#[test]
fn non_executable_backends_fail_with_every_cause() {
    let root = package(false);
    let check = check_backends(&backends(root.path(), true, true));
    assert!(!check.ok);
    assert_eq!(
        check
            .detail
            .matches("is not an executable regular file")
            .count(),
        2,
        "{}",
        check.detail
    );
    assert!(check.detail.contains("mprime binary"), "{}", check.detail);
}

#[test]
fn ready_backends_pass_and_name_their_binaries() {
    let root = package(true);
    let check = check_backends(&backends(root.path(), true, true));
    assert!(check.ok, "{}", check.detail);
    assert!(check.detail.starts_with("mprime: "), "{}", check.detail);
    assert!(check.detail.contains("; ycruncher: "), "{}", check.detail);
    assert!(
        check.detail.contains("00-x64 ~ baseline"),
        "{}",
        check.detail
    );
    assert!(check.detail.contains("24-ZN5 ~ Zen 5"), "{}", check.detail);
}

#[test]
fn a_resolved_account_reports_its_ids() {
    let account = Account::Resolved {
        name: "trial-user".to_owned(),
        identity: BackendIdentity::new(123, 456).unwrap(),
    };
    let check = account.check();
    assert!(check.ok);
    assert_eq!(check.name, "backend_user");
    assert_eq!(check.detail, "trial-user: uid 123 gid 456");
    assert_eq!(account.identity().unwrap().uid(), 123);
}

#[test]
fn an_unconfigured_account_is_refused_and_provides_no_identity() {
    let account = Account::resolve(None);
    let check = account.check();
    assert!(!check.ok);
    assert_eq!(check.detail, "no backend account is configured");
    assert!(account.identity().is_none());
}

#[test]
fn an_empty_account_name_is_refused_without_a_lookup() {
    let account = Account::resolve(Some(""));
    assert!(matches!(
        account,
        Account::Refused(IdentityError::EmptyName)
    ));
    assert!(account.identity().is_none());
}
