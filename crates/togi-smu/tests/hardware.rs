#![cfg(all(feature = "hardware", target_os = "linux"))]

use std::error::Error;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;
use togi_hostlock::{DEFAULT_PATH, HostLock, Policy};
use togi_machine::{CoreId, ObservedOffset, PmTableReader, Smu, TargetOffset, error_chain};
use togi_smu::Driver;

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

#[expect(
    clippy::disallowed_methods,
    reason = "hardware test: writes under the host lock and restores every original offset with a readback"
)]
fn write(
    driver: &mut Driver,
    core: CoreId,
    target: TargetOffset,
) -> Result<(), togi_machine::Error> {
    driver.set_offset(core, target)
}

/// Writes every original offset back and reads each one to confirm. One bounded pass: each
/// transaction has the driver's own deadline, and a poisoned driver fails the rest at once.
fn restore(driver: &mut Driver, originals: &[(CoreId, ObservedOffset)]) -> Vec<String> {
    let mut failures = Vec::new();
    for (core, original) in originals {
        let Ok(target) = TargetOffset::new(original.0) else {
            failures.push(format!(
                "core {core:02}: original {} is not writable",
                original.0
            ));
            continue;
        };
        if let Err(error) = write(driver, *core, target) {
            failures.push(format!("restore core {core:02}: {}", error_chain(&error)));
            continue;
        }
        match driver.offset(*core) {
            Ok(read) if read == *original => {}
            Ok(read) => failures.push(format!(
                "core {core:02}: restored {}, read back {}",
                original.0, read.0
            )),
            Err(error) => {
                failures.push(format!("read back core {core:02}: {}", error_chain(&error)));
            }
        }
    }
    failures
}

struct Restoring {
    driver: Driver,
    originals: Vec<(CoreId, ObservedOffset)>,
}

impl Drop for Restoring {
    fn drop(&mut self) {
        let failures = restore(&mut self.driver, &self.originals);
        if failures.is_empty() {
            return;
        }
        let report = failures.join("; ");
        if thread::panicking() {
            let _ = writeln!(io::stderr(), "offsets not restored: {report}");
        } else {
            panic!("offsets not restored: {report}");
        }
    }
}

#[test]
fn hardware_smu_round_trips_every_offset_and_restores_them() {
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
    let mut originals = Vec::with_capacity(cores.len());
    for info in &cores {
        let offset = must(
            driver.offset(info.core),
            &format!("read core {:02}", info.core),
        );
        assert!(
            TargetOffset::new(offset.0).is_ok(),
            "core {:02} reads {}, outside the writable range: writing it back would change it",
            info.core,
            offset.0
        );
        originals.push((info.core, offset));
    }
    let mut session = Restoring { driver, originals };
    for (core, original) in session.originals.clone() {
        let target = must(
            TargetOffset::new(original.0),
            &format!("core {core:02} target"),
        );
        must(
            write(&mut session.driver, core, target),
            &format!("write core {core:02}"),
        );
        let read = must(session.driver.offset(core), &format!("read core {core:02}"));
        assert_eq!(
            read, original,
            "core {core:02} wrote {}, read back {}",
            original.0, read.0
        );
        println!("core {core:02}: wrote {}, read back {}", original.0, read.0);
    }
    let context = must(session.driver.bios_context(), "read the BIOS context");
    assert!(
        context.boost_limit_mhz > 0,
        "boost limit {} MHz",
        context.boost_limit_mhz
    );
    println!("BIOS context: {context:?}");
}

#[test]
fn hardware_pm_table_lanes_decode_and_residencies_sum_to_100() {
    let _serial = exclusive();
    let mut driver = validated_driver();
    let reader = must(driver.pm_reader(), "open the PM table reader");
    let _ = reader.check();
    thread::sleep(Duration::from_millis(5));
    let check = reader.check();
    let table = reader
        .pm_table()
        .unwrap_or_else(|| panic!("{}", check.detail));
    for core in 0..table.c0_pct.len() {
        let sum = f64::from(table.c0_pct[core])
            + f64::from(table.cc1_pct[core])
            + f64::from(table.cc6_pct[core]);
        assert!(
            (sum - 100.0).abs() <= 1.0,
            "core {core:02} C0+CC1+CC6 = {sum:.4}%, want 100% within 1%"
        );
        println!(
            "core {core:02}: {:.4} W, {:.4} V requested, {:.2} C, C0 {:.3}% CC1 {:.3}% CC6 {:.3}%",
            table.power_w[core],
            table.voltage_request_v[core],
            table.temperature_c[core],
            table.c0_pct[core],
            table.cc1_pct[core],
            table.cc6_pct[core],
        );
    }
}
