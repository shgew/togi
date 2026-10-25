use togi_machine::{Check, Host};

use super::support::{Scope, Tree, Watch, host, refused_account, resolved_account};
use crate::host::Privilege;

fn names(checks: &[Check]) -> Vec<&str> {
    checks.iter().map(|check| check.name.as_str()).collect()
}

fn find<'a>(checks: &'a [Check], name: &str) -> &'a Check {
    checks.iter().find(|check| check.name == name).unwrap()
}

const ALL_CHECKS: [&str; 9] = [
    "root",
    "cpu",
    "ryzen_smu",
    "readback",
    "pm_table",
    "slot_mapping",
    "backends",
    "backend_user",
    "systemd_run",
];

mod unvalidated {
    use super::*;
    use crate::tests::support::Watch;

    #[test]
    fn an_unsupported_cpu_and_a_missing_driver_skip_every_gated_check_and_touch_no_hardware() {
        let tree = Tree::new();
        tree.put(
            "proc/cpuinfo",
            "cpu family : 25\nmodel : 68\nmodel name : Test CPU\n",
        );
        tree.remove_dir("sys/kernel/ryzen_smu_drv");
        let mut host = host(&tree, Privilege::Root, refused_account(), Scope::creating());
        let checks = host.preflight();
        assert_eq!(names(&checks), ALL_CHECKS);
        assert!(!find(&checks, "cpu").ok);
        assert!(find(&checks, "cpu").detail.contains("not Granite Ridge"));
        assert!(!find(&checks, "ryzen_smu").ok);
        assert!(
            find(&checks, "ryzen_smu")
                .detail
                .contains("ryzen_smu is not loaded")
        );
        for name in [
            "readback",
            "pm_table",
            "slot_mapping",
            "backends",
            "backend_user",
            "systemd_run",
        ] {
            let check = find(&checks, name);
            assert!(!check.ok, "{name}");
            assert_eq!(
                check.detail, "needs passing cpu and ryzen_smu checks",
                "{name}"
            );
        }
    }

    #[test]
    fn a_supported_cpu_without_the_driver_is_refused_before_any_mailbox_access() {
        let tree = Tree::new();
        let watch = Watch::on(&tree);
        tree.put("sys/kernel/ryzen_smu_drv/codename", "99\n");
        let mut host = host(
            &tree,
            Privilege::Root,
            resolved_account(),
            Scope::creating(),
        );
        let checks = host.preflight();
        assert!(find(&checks, "cpu").ok);
        assert!(!find(&checks, "ryzen_smu").ok);
        assert!(find(&checks, "ryzen_smu").detail.contains("codename 99"));
        assert_eq!(
            find(&checks, "readback").detail,
            "needs passing cpu and ryzen_smu checks"
        );
        assert_eq!(watch.touched(), Vec::<&str>::new());
    }

    #[test]
    fn a_missing_cpuinfo_is_refused_before_any_mailbox_access() {
        let tree = Tree::new();
        let watch = Watch::on(&tree);
        tree.remove("proc/cpuinfo");
        let mut host = host(
            &tree,
            Privilege::Root,
            resolved_account(),
            Scope::creating(),
        );
        let checks = host.preflight();
        assert!(!find(&checks, "cpu").ok);
        assert!(find(&checks, "cpu").detail.contains("cpuinfo"));
        assert!(find(&checks, "ryzen_smu").ok);
        assert_eq!(
            find(&checks, "slot_mapping").detail,
            "needs passing cpu and ryzen_smu checks"
        );
        assert_eq!(watch.touched(), Vec::<&str>::new());
    }

    #[test]
    fn privileged_checks_are_refused_without_root_and_touch_no_hardware() {
        let tree = Tree::new();
        let watch = Watch::on(&tree);
        let scope = Scope::creating();
        let mut host = host(
            &tree,
            Privilege::User(1000),
            resolved_account(),
            scope.clone(),
        );
        let checks = host.preflight();
        assert_eq!(names(&checks), ALL_CHECKS);
        assert!(!find(&checks, "root").ok);
        assert_eq!(
            find(&checks, "root").detail,
            "running as uid 1000; run needs root"
        );
        assert!(find(&checks, "cpu").ok);
        assert!(find(&checks, "ryzen_smu").ok);
        for name in ["readback", "pm_table", "slot_mapping", "systemd_run"] {
            let check = find(&checks, name);
            assert!(!check.ok, "{name}");
            assert_eq!(check.detail, "needs root", "{name}");
        }
        assert_eq!(scope.calls.get(), 0);
        assert_eq!(watch.touched(), Vec::<&str>::new());
    }

    #[test]
    fn validating_the_smu_is_refused_without_root_and_touches_no_hardware() {
        let tree = Tree::new();
        let watch = Watch::on(&tree);
        let mut host = host(
            &tree,
            Privilege::User(1000),
            resolved_account(),
            Scope::creating(),
        );
        let error = host.validate_smu().unwrap_err();
        assert!(error.to_string().contains("needs root"), "{error}");
        assert_eq!(watch.touched(), Vec::<&str>::new());
    }

    #[test]
    fn root_with_a_valid_identity_reaches_the_mailbox_and_reports_a_refused_account() {
        let tree = Tree::new();
        let watch = Watch::on(&tree);
        let scope = Scope::creating();
        let mut host = host(&tree, Privilege::Root, refused_account(), scope.clone());
        let checks = host.preflight();
        assert_eq!(names(&checks), ALL_CHECKS);
        assert!(find(&checks, "root").ok);
        assert_eq!(find(&checks, "root").detail, "uid 0");
        assert!(find(&checks, "cpu").ok);
        assert!(find(&checks, "ryzen_smu").ok);
        assert!(!find(&checks, "readback").ok);
        assert!(!find(&checks, "slot_mapping").ok);
        assert!(find(&checks, "pm_table").ok);
        assert!(!find(&checks, "backends").ok);
        let user = find(&checks, "backend_user");
        assert!(!user.ok);
        assert_eq!(user.detail, "no backend account is configured");
        let systemd = find(&checks, "systemd_run");
        assert!(!systemd.ok);
        assert!(
            systemd
                .detail
                .starts_with("systemd-run needs a usable backend_user:"),
            "{}",
            systemd.detail
        );
        assert_eq!(scope.calls.get(), 0);
        assert!(!watch.touched().is_empty());
    }
}

#[test]
fn a_pm_reader_worker_spawn_failure_is_informational() {
    let tree = Tree::new();
    let host = host(
        &tree,
        Privilege::Root,
        resolved_account(),
        Scope::creating(),
    );
    let check = host.check_pm_reader(Err(togi_smu::Error::WorkerSpawn(std::io::Error::other(
        "PM reader thread unavailable",
    ))));

    assert_eq!(check.name, "pm_table");
    assert!(check.ok, "PM table must not fail preflight: {check:?}");
    assert!(check.detail.contains("PM reader thread unavailable"));
    assert!(!host.pm.is_published());
}

#[test]
fn a_resolved_account_runs_the_scope_probe() {
    let tree = Tree::new();
    let scope = Scope::creating();
    let mut host = host(&tree, Privilege::Root, resolved_account(), scope.clone());
    let checks = host.preflight();
    let systemd = find(&checks, "systemd_run");
    assert!(systemd.ok);
    assert_eq!(systemd.detail, "scope confined to cpu 0 created");
    assert_eq!(scope.calls.get(), 1);
    assert_eq!(
        find(&checks, "backend_user").detail,
        "trial-user: uid 123 gid 456"
    );
}

#[test]
fn a_refused_scope_probe_fails_the_check_with_its_cause() {
    let tree = Tree::new();
    let mut host = host(
        &tree,
        Privilege::Root,
        resolved_account(),
        Scope::refusing("Failed to start transient scope unit"),
    );
    let checks = host.preflight();
    let systemd = find(&checks, "systemd_run");
    assert!(!systemd.ok);
    assert_eq!(
        systemd.detail,
        "the preflight scope probe failed: exited with exit status: 1: \
         Failed to start transient scope unit"
    );
}

fn failed_probe_detail(error: fn() -> togi_trial::Error) -> String {
    let tree = Tree::new();
    let mut host = host(
        &tree,
        Privilege::Root,
        resolved_account(),
        Scope::failing(error),
    );
    let checks = host.preflight();
    let systemd = find(&checks, "systemd_run");
    assert!(!systemd.ok);
    systemd.detail.clone()
}

#[test]
fn a_panicked_trial_runner_fails_the_scope_check_naming_the_panic() {
    let detail = failed_probe_detail(|| togi_trial::Error::Panicked {
        what: "the probe task met BOOM".to_owned(),
    });
    assert_eq!(
        detail,
        "trial supervision panicked: the probe task met BOOM"
    );
}

#[test]
fn a_stuck_trial_runner_fails_the_scope_check_naming_the_bound() {
    let detail = failed_probe_detail(|| togi_trial::Error::Stuck {
        operation: "the scope probe",
        bound: std::time::Duration::from_secs(35),
    });
    assert_eq!(
        detail,
        "the trial runtime did not answer the scope probe within 35s"
    );
}

#[test]
fn a_vanished_trial_runtime_fails_the_scope_check() {
    let detail = failed_probe_detail(|| togi_trial::Error::RuntimeGone);
    assert_eq!(detail, "the trial runtime thread is no longer running");
}

#[test]
fn the_host_reads_its_topology_from_the_one_driver() {
    let tree = Tree::new();
    let mut host = host(&tree, Privilege::Root, refused_account(), Scope::creating());
    let cores = host.topology().unwrap();
    assert_eq!(cores.len(), 1);
    assert_eq!(cores[0].cpus.len(), 2);
}

#[test]
fn the_host_watchdog_reads_its_root() {
    let tree = Tree::new();
    let mut host = host(&tree, Privilege::Root, refused_account(), Scope::creating());
    tree.put("sys/class/watchdog/watchdog0/state", "active\n");
    tree.put(
        "sys/class/watchdog/watchdog0/identity",
        "SP5100 TCO timer\n",
    );
    tree.put("sys/class/watchdog/watchdog0/timeout", "60\n");
    tree.put("sys/class/watchdog/watchdog0/timeleft", "58\n");
    let check = host.watchdog();
    assert!(check.ok, "{check:?}");
}

#[test]
fn the_host_ranking_reads_each_cores_first_cpu() {
    let tree = Tree::new();
    tree.put(
        "sys/devices/system/cpu/cpufreq/policy0/amd_pstate_prefcore_ranking",
        "201\n",
    );
    let mut host = host(&tree, Privilege::Root, refused_account(), Scope::creating());
    let ranks = host.ranking().unwrap();
    assert_eq!(ranks.len(), 1);
    assert_eq!(ranks[0].value, 201);
}

#[test]
fn an_unreadable_ranking_is_a_partial_without_values() {
    let tree = Tree::new();
    let mut host = host(&tree, Privilege::Root, refused_account(), Scope::creating());
    let partial = host.ranking().unwrap_err();
    assert!(partial.value.is_empty());
}

#[test]
fn validating_the_smu_as_root_validates_the_driver_and_publishes_the_pm_reader() {
    let tree = Tree::new();
    let mut host = host(&tree, Privilege::Root, refused_account(), Scope::creating());
    assert!(!host.pm.is_published());
    let _proof = host.validate_smu().unwrap();
    assert!(host.pm.is_published());
}

#[test]
fn an_invalid_identity_fails_validation_publishes_nothing_and_touches_no_hardware() {
    let tree = Tree::new();
    let watch = Watch::on(&tree);
    tree.put("sys/kernel/ryzen_smu_drv/codename", "99\n");
    let mut host = host(&tree, Privilege::Root, refused_account(), Scope::creating());
    let error = host.validate_smu().unwrap_err();
    assert!(error.to_string().contains("codename 99"), "{error}");
    assert!(!host.pm.is_published());
    assert_eq!(watch.touched(), Vec::<&str>::new());
}

#[test]
fn a_supported_cpu_with_no_driver_loaded_is_refused_and_skips_every_gated_check() {
    let tree = Tree::new();
    tree.remove_dir("sys/kernel/ryzen_smu_drv");
    let scope = Scope::creating();
    let mut host = host(&tree, Privilege::Root, resolved_account(), scope.clone());
    let checks = host.preflight();
    assert_eq!(names(&checks), ALL_CHECKS);
    assert!(find(&checks, "root").ok);
    assert!(find(&checks, "cpu").ok);
    let driver = find(&checks, "ryzen_smu");
    assert!(!driver.ok);
    assert!(
        driver.detail.contains("ryzen_smu is not loaded"),
        "{}",
        driver.detail
    );
    for name in &ALL_CHECKS[3..] {
        let check = find(&checks, name);
        assert!(!check.ok, "{name}");
        assert_eq!(
            check.detail, "needs passing cpu and ryzen_smu checks",
            "{name}"
        );
    }
    assert_eq!(scope.calls.get(), 0);
}

#[test]
#[cfg(target_os = "linux")]
fn real_fs_identity_refusal_does_not_open_mailbox_or_pm_files() {
    use std::mem::MaybeUninit;

    use rustix::fs::inotify;

    use super::support::MAILBOX_AND_PM_FILES;
    use crate::assemble::Parts;

    let tree = Tree::new();
    tree.put("sys/kernel/ryzen_smu_drv/codename", "99\n");
    let dir = tree.materialize();
    let inotify =
        inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC).unwrap();
    for name in MAILBOX_AND_PM_FILES {
        inotify::add_watch(
            &inotify,
            dir.path().join("sys/kernel/ryzen_smu_drv").join(name),
            inotify::WatchFlags::ALL_EVENTS,
        )
        .unwrap();
    }
    let driver = togi_smu::Driver::open(dir.path()).unwrap();
    let parts = Parts::new(driver, &togi_config::Config::default()).unwrap();
    let mut host = parts.host(dir.path(), Privilege::Root, Scope::creating());
    let checks = host.preflight();
    assert!(!find(&checks, "ryzen_smu").ok);
    assert_eq!(
        find(&checks, "readback").detail,
        "needs passing cpu and ryzen_smu checks"
    );
    assert!(host.validate_smu().is_err());
    let mut buffer = [MaybeUninit::uninit(); 1024];
    assert!(matches!(
        inotify::Reader::new(&inotify, &mut buffer).next(),
        Err(rustix::io::Errno::AGAIN)
    ));
    std::fs::read(dir.path().join("sys/kernel/ryzen_smu_drv/pm_table")).unwrap();
    assert!(
        inotify::Reader::new(&inotify, &mut buffer).next().is_ok(),
        "the watch must observe a real open/read"
    );
}
