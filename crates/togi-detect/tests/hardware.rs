#![cfg(all(feature = "hardware", target_os = "linux"))]

use std::error::Error;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use togi_command::Invocation;
use togi_detect::Kernel;
use togi_hostlock::{DEFAULT_PATH, HostLock, Policy};
use togi_machine::{BootId, CcdId, CoreId, CoreInfo, CpuId, Kernel as _};

const UNKNOWN_BOOT: &str = "0123456789abcdef0123456789abcdef";
const SETUP_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Boot {
    index: i64,
    boot_id: String,
}

fn listed_boots() -> Result<Vec<Boot>, Box<dyn Error>> {
    let invocation =
        Invocation::new("journalctl").args(["--list-boots", "-o", "json", "--no-pager", "-q"]);
    #[expect(
        clippy::disallowed_methods,
        reason = "the hardware test lists the host's boots with the same approved command as detect"
    )]
    let output = togi_command::run_blocking(&invocation, SETUP_DEADLINE)?;
    if !output.status.success() {
        return Err(format!("journalctl --list-boots: {}", output.status).into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn uptime() -> Result<Duration, Box<dyn Error>> {
    let text = std::fs::read_to_string("/proc/uptime")?;
    let seconds = text.split_whitespace().next().ok_or("empty /proc/uptime")?;
    Ok(Duration::from_secs_f64(seconds.parse()?))
}

#[test]
fn the_host_journal_answers_for_its_boots() -> Result<(), Box<dyn Error>> {
    let _lock = HostLock::acquire(Path::new(DEFAULT_PATH), Policy::Host)?;
    let boots = listed_boots()?;
    if boots.iter().any(|boot| boot.boot_id == UNKNOWN_BOOT) {
        return Err(format!(
            "boot {UNKNOWN_BOOT} is in the system journal; the unknown-boot case needs an id it does not list"
        )
        .into());
    }
    let previous = boots
        .iter()
        .find(|boot| boot.index == -1)
        .ok_or("no previous boot in the system journal")?;
    let current = togi_detect::boot_id()?;

    let topology = [CoreInfo {
        core: CoreId::new(0)?,
        ccd: CcdId(0),
        cpus: vec![CpuId(0)],
    }];
    let mut kernel = Kernel::new(&topology)?;

    let before = kernel.mces(&BootId::from(previous.boot_id.clone()), Duration::ZERO)?;
    println!("previous boot: {} machine checks", before.len());
    let last_hour = kernel.mces(
        &current,
        uptime()?.saturating_sub(Duration::from_secs(3600)),
    )?;
    println!(
        "current boot, last hour: {} machine checks",
        last_hour.len()
    );
    let unknown = kernel.mces(&BootId::from(UNKNOWN_BOOT.to_owned()), Duration::ZERO);
    assert!(
        matches!(
            unknown,
            Err(togi_machine::Partial {
                error: togi_machine::Error::BootMissing,
                ..
            })
        ),
        "{unknown:?}"
    );

    let reason = kernel.reset_reason(&current)?;
    println!("current boot's reset reason: {reason:?}");

    let first = kernel.read_mces(&current, None)?;
    let resumed = kernel.read_mces(&current, Some(&first.cursor))?;
    assert!(!resumed.cursor.as_str().is_empty());
    Ok(())
}
