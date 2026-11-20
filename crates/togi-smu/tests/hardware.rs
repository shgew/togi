#![cfg(all(feature = "hardware", target_os = "linux"))]

use std::error::Error;
use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;
use togi_hostlock::{DEFAULT_PATH, HostLock, Policy};
use togi_machine::{PmTableReader, error_chain};
use togi_smu::{Driver, OffsetWitness, WitnessError};

#[path = "../src/tests/witness_guard.rs"]
mod witness_guard;
use witness_guard::Restoring;

/// Taken on first use, held until process exit and never dropped: a transaction past its
/// deadline leaves its worker inside an uncancellable sysfs call that may still complete a
/// write, so the kernel must release the lock only once every thread of this process, a stuck
/// worker included, is gone. Another togi process holding it still fails the test with Busy.
static HOST_LOCK: LazyLock<HostLock> = LazyLock::new(|| {
    must(
        HostLock::acquire(Path::new(DEFAULT_PATH), Policy::Host),
        "take the host lock",
    )
});
static SERIAL: Mutex<()> = Mutex::new(());

fn must<T, E: Error>(result: Result<T, E>, what: &str) -> T {
    result.unwrap_or_else(|error| panic!("{what}: {}", error_chain(&error)))
}

/// Runs the hardware tests of this process one at a time, behind the process-lifetime host lock.
fn exclusive() -> MutexGuard<'static, ()> {
    // A panicked test may have left its worker inside an uncancellable sysfs write: refuse, never recover.
    let Ok(serial) = SERIAL.lock() else {
        panic!(
            "an earlier hardware test in this process panicked; its SMU worker may still be inside \
             an uncancellable sysfs call, so this process refuses further hardware access"
        );
    };
    LazyLock::force(&HOST_LOCK);
    serial
}

fn validated_driver() -> Driver {
    let mut driver = must(Driver::system(), "open the SMU driver");
    let _ = must(driver.validate(), "validate the SMU interface");
    driver
}

#[test]
fn hardware_smu_round_trips_sixteen_distinct_offsets_and_restores_stock() {
    let _serial = exclusive();
    let mut driver = validated_driver();
    for check in [
        driver.check_cpu(),
        driver.check_driver(),
        driver.check_slot_mapping(),
        driver.check_readback(),
    ] {
        assert!(check.ok, "check {} failed: {}", check.name, check.detail);
        println!("{}: {}", check.name, check.detail);
    }
    let cores = must(driver.topology(), "read the topology");
    let witness = must(
        OffsetWitness::fixture(cores.iter().map(|info| info.core)),
        "prepare sixteen distinct offsets",
    );
    // Armed only now: a refused precheck or topology must not write to a machine it rejected.
    let mut session = Restoring::new(driver);
    let readback = must(session.round_trip(&witness), "round trip distinct offsets");
    for (core, target) in witness.targets().iter() {
        println!(
            "core {core:02}: wrote {}, read back {:?}",
            target.get(),
            readback.get(core)
        );
    }
    let context = must(session.driver().bios_context(), "read the BIOS context");
    assert!(
        context.boost_limit_mhz > 0,
        "boost limit {} MHz",
        context.boost_limit_mhz
    );
    println!("BIOS context: {context:?}");
    let failures = session.finish();
    assert!(
        failures.is_empty(),
        "stock not verified: {}",
        failures.join("; ")
    );
    println!("stock restored and verified on all sixteen cores");
}

#[test]
fn hardware_pm_table_lanes_decode() {
    let _serial = exclusive();
    let mut driver = validated_driver();
    let reader = must(driver.pm_reader(), "open the PM table reader");
    let _ = reader.check();
    thread::sleep(Duration::from_millis(5));
    let check = reader.check();
    let table = reader
        .pm_table()
        .unwrap_or_else(|| panic!("{}", check.detail));
    for core in 0..table.power_w.len() {
        println!(
            "core {core:02}: {:.4} W, {:.4} V requested, {:.2} C",
            table.power_w[core], table.voltage_request_v[core], table.temperature_c[core],
        );
    }
}
