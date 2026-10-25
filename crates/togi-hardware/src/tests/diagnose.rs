use togi_machine::{BiosContext, Check, Host};

use super::support::{
    Scope, Tree, Watch, host, host_with_driver, refused_account, resolved_account,
};
use crate::adapters::lock;
use crate::checks::{CheckName, Skip, Skipped};
use crate::diagnose::{Diagnosis, diagnose_with, diagnosis_host_with};
use crate::error::Error;
use crate::host::{Privilege, SystemHost};

fn diagnosis_host(
    tree: &Tree,
    request_privileged: bool,
    privilege: Privilege,
) -> Result<(SystemHost<Tree>, bool), Error> {
    diagnosis_host_with(
        request_privileged,
        privilege,
        || Ok(tree.driver(false)),
        || Ok(tree.driver(true)),
        |driver| {
            Ok(host_with_driver(
                tree,
                driver,
                privilege,
                refused_account(),
                Scope::creating(),
            ))
        },
    )
}

fn diagnose_at(
    tree: &Tree,
    recorded: Option<&BiosContext>,
    request_privileged: bool,
    privilege: Privilege,
) -> Result<Diagnosis, Error> {
    let (host, privileged) = diagnosis_host(tree, request_privileged, privilege)?;
    Ok(diagnose_with(&host, recorded, privileged))
}

fn names(checks: &[Check]) -> Vec<&str> {
    checks.iter().map(|check| check.name.as_str()).collect()
}

fn recorded() -> BiosContext {
    BiosContext {
        bios_version: "3.14".to_owned(),
        ..BiosContext::default()
    }
}

#[test]
fn an_unprivileged_diagnosis_touches_no_mailbox_or_pm_table_file() {
    let tree = Tree::new();
    let watch = Watch::on(&tree);
    let diagnosis = diagnose_at(&tree, Some(&recorded()), false, Privilege::Root).unwrap();
    assert_eq!(watch.touched(), Vec::<&str>::new());
    assert_eq!(
        names(&diagnosis.ran),
        ["cpu", "ryzen_smu", "backends", "backend_user", "watchdog"]
    );
    assert!(
        diagnosis.ran[0].ok && diagnosis.ran[1].ok,
        "{:?}",
        &diagnosis.ran[..2]
    );
    let skipped: Vec<_> = [
        CheckName::Readback,
        CheckName::PmTable,
        CheckName::SlotMapping,
        CheckName::SystemdRun,
        CheckName::BiosContext,
    ]
    .into_iter()
    .map(|name| Skipped {
        name,
        reason: Skip::NeedsRoot,
    })
    .collect();
    assert_eq!(diagnosis.skipped, skipped);
    assert!(diagnosis.bios_context_error.is_none());
}

#[test]
fn an_unprivileged_diagnosis_of_an_unidentified_machine_skips_only_what_needs_root() {
    let tree = Tree::new();
    tree.put(
        "proc/cpuinfo",
        "cpu family : 25\nmodel : 68\nmodel name : Test CPU\n",
    );
    let diagnosis = diagnose_at(&tree, None, false, Privilege::User(1000)).unwrap();
    assert!(!diagnosis.ran[0].ok);
    assert_eq!(names(&diagnosis.ran), ["cpu", "ryzen_smu", "watchdog"]);
    let reasons: Vec<_> = diagnosis
        .skipped
        .iter()
        .map(|skipped| (skipped.name.as_str(), skipped.reason))
        .collect();
    assert_eq!(
        reasons,
        [
            ("readback", Skip::NeedsRoot),
            ("pm_table", Skip::NeedsRoot),
            ("slot_mapping", Skip::NeedsRoot),
            ("backends", Skip::NeedsIdentity),
            ("backend_user", Skip::NeedsIdentity),
            ("systemd_run", Skip::NeedsRoot),
        ]
    );
}

#[test]
fn the_privileged_and_unprivileged_check_sets_partition_the_preflight() {
    let tree = Tree::new();
    let mut system = host(&tree, Privilege::Root, refused_account(), Scope::creating());
    let preflight = system.preflight();
    assert_eq!(preflight[0].name, "root");
    // The doctor also reports backend availability, which `run` probes per backend apart from its preflight.
    let mut steps = names(&preflight[1..]);
    let user = steps
        .iter()
        .position(|name| *name == "backend_user")
        .unwrap();
    steps.insert(user, "backends");

    let watch = Watch::on(&tree);
    let privileged = diagnose_at(&tree, None, true, Privilege::Root).unwrap();
    assert!(!watch.touched().is_empty());
    assert!(privileged.skipped.is_empty());
    let mut expected = steps.clone();
    expected.push("watchdog");
    assert_eq!(names(&privileged.ran), expected);

    let unprivileged = diagnose_at(&tree, None, false, Privilege::Root).unwrap();
    let (watchdog, ran) = unprivileged.ran.split_last().unwrap();
    assert_eq!(watchdog.name, "watchdog");
    let mut union: Vec<&str> = names(ran);
    union.extend(
        unprivileged
            .skipped
            .iter()
            .map(|skipped| skipped.name.as_str()),
    );
    union.sort_unstable();
    let mut all = steps;
    all.sort_unstable();
    assert_eq!(union, all);
}

#[test]
fn failed_checks_skip_the_bios_comparison() {
    let tree = Tree::new();
    let diagnosis = diagnose_at(&tree, Some(&recorded()), true, Privilege::Root).unwrap();
    assert!(!names(&diagnosis.ran).contains(&"bios_context"));
    assert_eq!(
        diagnosis.skipped,
        [Skipped {
            name: CheckName::BiosContext,
            reason: Skip::NeedsOtherChecks,
        }]
    );
    assert!(diagnosis.bios_context_error.is_none());
}

/// Every step passes but the named ones.
struct Failing(&'static [CheckName]);

impl crate::diagnose::Survey for Failing {
    fn steps(&self, _: bool) -> Vec<crate::preflight::Outcome> {
        crate::preflight::STEPS
            .into_iter()
            .map(|step| {
                crate::preflight::Outcome::Ran(Check {
                    name: step.name().as_str().to_owned(),
                    detail: String::new(),
                    ok: !self.0.contains(&step.name()),
                })
            })
            .collect()
    }

    fn watchdog(&self) -> Check {
        Check {
            name: "watchdog".to_owned(),
            detail: String::new(),
            ok: true,
        }
    }

    fn bios_context(&self) -> Result<BiosContext, togi_smu::Error> {
        Ok(recorded())
    }
}

#[test_case::test_case(&[CheckName::Backends], true; "an unavailable backend")]
#[test_case::test_case(&[CheckName::BackendUser], false; "a mandatory check")]
fn only_a_failed_mandatory_check_skips_the_bios_comparison(
    failing: &'static [CheckName],
    compared: bool,
) {
    let diagnosis = diagnose_with(&Failing(failing), Some(&recorded()), true);
    assert_eq!(names(&diagnosis.ran).contains(&"bios_context"), compared);
}

#[test]
fn a_privileged_request_by_a_user_stays_identity_only_and_skips_every_privileged_probe() {
    assert_identity_only_diagnosis(true, Privilege::User(1000));
}

#[test]
fn root_without_a_privileged_request_stays_identity_only_and_skips_every_privileged_probe() {
    assert_identity_only_diagnosis(false, Privilege::Root);
}

fn assert_identity_only_diagnosis(request_privileged: bool, privilege: Privilege) {
    let tree = Tree::new();
    let watch = Watch::on(&tree);
    let diagnosis = diagnose_at(&tree, Some(&recorded()), request_privileged, privilege).unwrap();
    assert_eq!(watch.touched(), Vec::<&str>::new());
    assert_eq!(
        names(&diagnosis.ran),
        ["cpu", "ryzen_smu", "backends", "backend_user", "watchdog"]
    );
    let skipped: Vec<_> = [
        CheckName::Readback,
        CheckName::PmTable,
        CheckName::SlotMapping,
        CheckName::SystemdRun,
        CheckName::BiosContext,
    ]
    .into_iter()
    .map(|name| Skipped {
        name,
        reason: Skip::NeedsRoot,
    })
    .collect();
    assert_eq!(diagnosis.skipped, skipped);
    assert!(diagnosis.bios_context_error.is_none());

    let (mut host, privileged) = diagnosis_host(&tree, request_privileged, privilege).unwrap();
    assert!(!privileged);
    let scope = Scope::creating();
    host.scope = Box::new(scope.clone());
    host.account = resolved_account();
    let diagnosis = diagnose_with(&host, Some(&recorded()), privileged);
    assert_eq!(diagnosis.skipped, skipped);
    assert_eq!(scope.calls.get(), 0);

    let mut driver = lock(&host.driver);
    assert!(matches!(
        driver.validate(),
        Err(togi_smu::Error::IdentityOnly { .. })
    ));
    assert!(matches!(
        driver.pm_reader(),
        Err(togi_smu::Error::IdentityOnly { .. })
    ));
    assert!(!driver.check_readback().ok);
    assert_eq!(watch.touched(), Vec::<&str>::new());
}
