use std::path::Path;
use std::sync::Arc;

use test_case::test_case;
use togi_backend::{Backend as BackendAdapter, Error as BackendError, Line, Plan, Ready};
use togi_machine::{Backend, Check, CpuId, Workload};
use togi_trial::{BackendIdentity, Backends, IdentityError};

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

#[test_case(Step::Cpu; "cpu")]
#[test_case(Step::RyzenSmu; "ryzen_smu")]
fn a_failed_identity_check_skips_every_check_that_needs_the_identity(failing: Step) {
    for (name, skipped, _) in outcomes(true, &[failing]) {
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

struct BackendCheck {
    kind: Backend,
    executable: bool,
}

impl BackendAdapter for BackendCheck {
    fn configuration(&self, workload: &Workload) -> Result<Vec<u8>, BackendError> {
        Ok(workload.id.as_bytes().to_vec())
    }

    fn kind(&self) -> Backend {
        self.kind
    }

    fn check(&self) -> Result<Ready, BackendError> {
        let (name, path, detail) = match self.kind {
            Backend::Mprime => ("mprime", "/fixture/bin/mprime", "/fixture/bin/mprime"),
            Backend::Ycruncher => (
                "y-cruncher",
                "/fixture/lib/y-cruncher/Binaries/00-x64 ~ baseline",
                "00-x64 ~ baseline; 24-ZN5 ~ Zen 5",
            ),
        };
        if self.executable {
            Ok(Ready {
                detail: detail.to_owned(),
            })
        } else {
            Err(BackendError::NotExecutable {
                name,
                path: path.into(),
            })
        }
    }

    fn plan(
        &self,
        _workload: &Workload,
        _dir: &Path,
        _cpus: &[CpuId],
    ) -> Result<Plan, BackendError> {
        panic!("preflight checks must not plan workloads")
    }

    fn classify(&self, _line: &str) -> Line {
        panic!("preflight checks must not classify workload output")
    }
}

fn backends(mprime: bool, ycruncher: bool, executable: bool) -> Backends {
    let mut backends = Backends::new();
    for (kind, configured) in [(Backend::Mprime, mprime), (Backend::Ycruncher, ycruncher)] {
        if configured {
            backends = backends.with(Arc::new(BackendCheck { kind, executable }));
        }
    }
    backends
}

#[test]
fn unconfigured_backends_fail_naming_both() {
    let check = check_backends(&Backends::new());
    assert!(!check.ok);
    assert_eq!(check.name, "backends");
    assert_eq!(
        check.detail,
        "backends.mprime not configured; backends.ycruncher not configured"
    );
}

#[test]
fn an_unconfigured_backend_fails_and_the_other_is_still_reported() {
    let check = check_backends(&backends(true, false, true));
    assert!(!check.ok);
    assert!(check.detail.starts_with("mprime: "), "{}", check.detail);
    assert!(
        check
            .detail
            .ends_with("; backends.ycruncher not configured"),
        "{}",
        check.detail
    );

    let check = check_backends(&backends(false, true, true));
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
fn failed_backend_checks_report_every_cause() {
    let check = check_backends(&backends(true, true, false));
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
fn ready_backend_checks_pass_and_name_their_binaries() {
    let check = check_backends(&backends(true, true, true));
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
