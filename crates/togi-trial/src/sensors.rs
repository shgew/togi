use std::path::PathBuf;
use std::time::Duration;

use togi_machine::{CoreId, CoreInfo};

/// Where the sensors live; tests point these at fixtures.
#[derive(Clone, Debug)]
pub(crate) struct Roots {
    pub(crate) hwmon: PathBuf,
    pub(crate) cpu: PathBuf,
    pub(crate) powercap: PathBuf,
    pub(crate) proc: PathBuf,
}

impl Default for Roots {
    fn default() -> Self {
        Self {
            hwmon: "/sys/class/hwmon".into(),
            cpu: "/sys/devices/system/cpu".into(),
            powercap: "/sys/class/powercap".into(),
            proc: "/proc".into(),
        }
    }
}

/// One tick's sensor readings, every absent one omitted rather than zeroed.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Readings {
    pub(crate) tctl_c: Option<i64>,
    pub(crate) tccd_c: Option<std::collections::BTreeMap<String, i64>>,
    pub(crate) core_mhz: Vec<(CoreId, i64)>,
    pub(crate) package_power_w: Option<f64>,
}

/// Reads temperatures, clocks of the loaded cores and package energy.
#[derive(Debug)]
pub(crate) struct Sensors {
    roots: Roots,
    loaded: Vec<CoreId>,
    topology: Vec<CoreInfo>,
}

impl Sensors {
    /// Takes the energy baseline now.
    pub(crate) fn new(roots: Roots, topology: Vec<CoreInfo>, loaded: Vec<CoreId>) -> Self {
        Self {
            roots,
            loaded,
            topology,
        }
    }

    /// `elapsed` is the time since the previous reading, for the power division.
    pub(crate) fn read(&mut self, elapsed: Duration) -> Readings {
        let _ = (elapsed, &self.roots, &self.loaded, &self.topology);
        unimplemented!()
    }
}
