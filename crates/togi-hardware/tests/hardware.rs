#![cfg(all(feature = "hardware", target_os = "linux"))]

use std::path::Path;
use std::sync::LazyLock;

use togi_config::Config;
use togi_hostlock::{DEFAULT_PATH, HostLock, Policy};
use togi_machine::{CoreId, ObservedOffset, Smu};
use togi_smu::Driver;

/// Taken on first use and held until process exit and never dropped: another togi process holding
/// it fails the test with `Busy`.
static HOST_LOCK: LazyLock<HostLock> = LazyLock::new(|| {
    HostLock::acquire(Path::new(DEFAULT_PATH), Policy::Host)
        .unwrap_or_else(|error| panic!("take the host lock: {}", togi_machine::error_chain(&error)))
});

fn offsets(driver: &mut Driver, cores: &[CoreId]) -> Vec<(CoreId, ObservedOffset)> {
    cores
        .iter()
        .map(|&core| {
            let offset = driver
                .offset(core)
                .unwrap_or_else(|error| panic!("read the offset of core {core}: {error}"));
            (core, offset)
        })
        .collect()
}

#[test]
fn diagnosing_leaves_every_offset_unchanged() {
    let lock = &*HOST_LOCK;
    let mut driver = Driver::system().unwrap();
    let _validated = driver.validate().unwrap();
    let cores: Vec<CoreId> = driver
        .topology()
        .unwrap()
        .iter()
        .map(|core| core.core)
        .collect();
    let before = offsets(&mut driver, &cores);

    let diagnosis = togi_hardware::diagnose(&Config::default(), None, Some(lock)).unwrap();
    for check in &diagnosis.ran {
        println!("{} ok={}: {}", check.name, check.ok, check.detail);
    }

    assert!(diagnosis.skipped.is_empty(), "{:?}", diagnosis.skipped);
    assert!(diagnosis.bios_context_error.is_none());
    assert_eq!(before, offsets(&mut driver, &cores));
}
