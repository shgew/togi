use togi_config::Config;
use togi_machine::{BiosContext, Check, Host};

use super::support::{Scope, Tree, Watch, host, refused_account};
use crate::checks::{CheckName, Skip, Skipped};
use crate::diagnose::diagnose_at;
use crate::host::Privilege;

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
    let diagnosis = diagnose_at(tree.root(), &Config::default(), Some(&recorded()), false).unwrap();
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
    let diagnosis = diagnose_at(tree.root(), &Config::default(), None, false).unwrap();
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
    let steps = names(&preflight[1..]);

    let privileged = diagnose_at(tree.root(), &Config::default(), None, true).unwrap();
    assert!(privileged.skipped.is_empty());
    let mut expected = steps.clone();
    expected.push("watchdog");
    assert_eq!(names(&privileged.ran), expected);

    let unprivileged = diagnose_at(tree.root(), &Config::default(), None, false).unwrap();
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
    let diagnosis = diagnose_at(tree.root(), &Config::default(), Some(&recorded()), true).unwrap();
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
