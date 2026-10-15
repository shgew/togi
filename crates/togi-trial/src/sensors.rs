use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use togi_machine::{CoreId, CoreInfo, PerCore};
use tokio::time::Instant;

use crate::procfs::Inputs;

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
    pub(crate) tccd_c: Option<BTreeMap<String, i64>>,
    pub(crate) core_mhz: PerCore<i64>,
    pub(crate) package_power_w: Option<f64>,
}

/// Reads temperatures, clocks of the loaded cores and package energy.
#[derive(Debug)]
pub(crate) struct Sensors {
    hwmon: PathBuf,
    powercap: PathBuf,
    clocks: Vec<(CoreId, PathBuf)>,
    energy: Option<i64>,
    energy_at: Instant,
    inputs: Inputs,
}

fn read_integer(inputs: &Inputs, path: &Path) -> Option<i64> {
    inputs.read_to_string(path).ok()?.trim().parse().ok()
}

impl Sensors {
    /// Takes the energy baseline at `now`, when timing begins.
    pub(crate) fn with_inputs(
        roots: &Roots,
        topology: &[CoreInfo],
        loaded: &[CoreId],
        now: Instant,
        inputs: Inputs,
    ) -> Self {
        let clocks = loaded
            .iter()
            .filter_map(|&core| {
                let first = topology
                    .iter()
                    .find(|info| info.core == core)?
                    .cpus
                    .first()?;
                Some((
                    core,
                    roots
                        .cpu
                        .join(format!("cpu{}", first.0))
                        .join("cpufreq/scaling_cur_freq"),
                ))
            })
            .collect();
        let powercap = roots.powercap.join("intel-rapl:0");
        Self {
            hwmon: roots.hwmon.clone(),
            energy: read_integer(&inputs, &powercap.join("energy_uj")),
            powercap,
            clocks,
            energy_at: now,
            inputs,
        }
    }

    pub(crate) fn read(&mut self, now: Instant) -> Readings {
        let (tctl_c, tccd_c) = temperatures(&self.inputs, &self.hwmon);
        let mut core_mhz = PerCore::default();
        for (core, path) in &self.clocks {
            if let Some(khz) = read_integer(&self.inputs, path).filter(|khz| *khz >= 0) {
                core_mhz.set(*core, khz / 1000);
            }
        }
        let energy = read_integer(&self.inputs, &self.powercap.join("energy_uj"));
        let package_power_w = self.power(energy, now);
        self.energy = energy;
        self.energy_at = now;
        Readings {
            tctl_c,
            tccd_c,
            core_mhz,
            package_power_w,
        }
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "microjoule deltas are far below 2^53"
    )]
    fn power(&self, energy: Option<i64>, now: Instant) -> Option<f64> {
        let (current, previous) = (energy?, self.energy?);
        let elapsed = now.checked_duration_since(self.energy_at)?;
        if elapsed.is_zero() {
            return None;
        }
        let mut delta = current - previous;
        if delta < 0 {
            let limit = read_integer(&self.inputs, &self.powercap.join("max_energy_range_uj"))?;
            if limit <= previous {
                return None;
            }
            delta += limit;
        }
        (delta >= 0).then(|| delta as f64 / 1e6 / elapsed.as_secs_f64())
    }
}

/// The first hwmon directory named `k10temp` or `zenpower`; it is not skipped for lacking sensors.
fn temperatures(inputs: &Inputs, root: &Path) -> (Option<i64>, Option<BTreeMap<String, i64>>) {
    let Ok(entries) = inputs.read_dir(root) else {
        return (None, None);
    };
    let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    dirs.sort();
    for dir in dirs {
        let name = inputs.read_to_string(&dir.join("name")).unwrap_or_default();
        if !matches!(name.trim(), "k10temp" | "zenpower") {
            continue;
        }
        let Ok(files) = inputs.read_dir(&dir) else {
            return (None, None);
        };
        let mut labels: Vec<PathBuf> = files
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("temp") && n.ends_with("_label"))
            })
            .collect();
        labels.sort();
        let mut tctl = None;
        let mut tccd = BTreeMap::new();
        for path in labels {
            let Ok(label) = inputs.read_to_string(&path) else {
                continue;
            };
            let label = label.trim();
            let is_ccd = label
                .strip_prefix("Tccd")
                .and_then(|n| n.parse::<i64>().ok())
                .is_some_and(|n| n >= 1);
            if label != "Tctl" && !is_ccd {
                continue;
            }
            let input = path.with_file_name(
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .replace("_label", "_input"),
            );
            let Some(millidegrees) = read_integer(inputs, &input) else {
                continue;
            };
            if label == "Tctl" {
                tctl = Some(millidegrees / 1000);
            } else {
                tccd.insert(label.to_owned(), millidegrees / 1000);
            }
        }
        return (tctl, (!tccd.is_empty()).then_some(tccd));
    }
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use togi_machine::{CcdId, CpuId};

    struct Fixture {
        fs: Arc<togi_journal::fs::MemFs>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                fs: Arc::new(togi_journal::fs::MemFs::new()),
            }
        }

        fn write(&self, relative: &str, text: &str) {
            self.fs
                .put(Path::new("/sensors").join(relative), text.as_bytes());
        }

        fn roots() -> Roots {
            Roots {
                hwmon: "/sensors/hwmon".into(),
                cpu: "/sensors/cpu".into(),
                powercap: "/sensors/powercap".into(),
                proc: "/sensors/proc".into(),
            }
        }

        fn inputs(&self) -> Inputs {
            Inputs::memory(self.fs.clone())
        }

        fn sensors(&self, topology: &[CoreInfo], loaded: &[CoreId], now: Instant) -> Sensors {
            Sensors::with_inputs(&Self::roots(), topology, loaded, now, self.inputs())
        }
    }

    fn core(n: u8) -> CoreId {
        CoreId::new(i64::from(n)).unwrap()
    }

    fn topology() -> Vec<CoreInfo> {
        vec![
            CoreInfo {
                core: core(0),
                ccd: CcdId(0),
                cpus: vec![CpuId(0), CpuId(8)],
            },
            CoreInfo {
                core: core(1),
                ccd: CcdId(0),
                cpus: vec![CpuId(1), CpuId(9)],
            },
        ]
    }

    #[test]
    fn k10temp_labels_map_to_whole_degrees_and_zenpower_tctl_beats_tdie() {
        let fixture = Fixture::new();
        fixture.write("hwmon/hwmon0/name", "nvme\n");
        fixture.write("hwmon/hwmon1/name", "zenpower\n");
        fixture.write("hwmon/hwmon1/temp1_label", "Tdie\n");
        fixture.write("hwmon/hwmon1/temp1_input", "70999\n");
        fixture.write("hwmon/hwmon1/temp2_label", "Tctl\n");
        fixture.write("hwmon/hwmon1/temp2_input", "75500\n");
        fixture.write("hwmon/hwmon1/temp3_label", "Tccd1\n");
        fixture.write("hwmon/hwmon1/temp3_input", "61000\n");
        fixture.write("hwmon/hwmon1/temp4_label", "Tccd0\n");
        fixture.write("hwmon/hwmon1/temp4_input", "1000\n");
        fixture.write("hwmon/hwmon1/temp5_label", "Tccd2\n");
        let (tctl, tccd) = temperatures(&fixture.inputs(), &Fixture::roots().hwmon);
        assert_eq!(tctl, Some(75));
        assert_eq!(tccd, Some(BTreeMap::from([("Tccd1".to_owned(), 61)])));
    }

    #[test]
    fn the_first_matching_hwmon_is_used_even_without_sensors() {
        let fixture = Fixture::new();
        fixture.write("hwmon/hwmon0/name", "k10temp\n");
        fixture.write("hwmon/hwmon1/name", "zenpower\n");
        fixture.write("hwmon/hwmon1/temp1_label", "Tctl\n");
        fixture.write("hwmon/hwmon1/temp1_input", "50000\n");
        assert_eq!(
            temperatures(&fixture.inputs(), &Fixture::roots().hwmon),
            (None, None)
        );
        assert_eq!(
            temperatures(&fixture.inputs(), Path::new("/sensors/absent")),
            (None, None)
        );
    }

    #[test]
    fn clocks_come_from_the_first_cpu_of_each_loaded_core_only() {
        let fixture = Fixture::new();
        fixture.write("cpu/cpu0/cpufreq/scaling_cur_freq", "4500000\n");
        fixture.write("cpu/cpu1/cpufreq/scaling_cur_freq", "3999999\n");
        fixture.write("cpu/cpu8/cpufreq/scaling_cur_freq", "1000\n");
        let now = Instant::now();
        let mut sensors = fixture.sensors(&topology(), &[core(0)], now);
        let readings = sensors.read(now + Duration::from_secs(1));
        assert_eq!(readings.core_mhz.get(core(0)), Some(4500));
        assert_eq!(readings.core_mhz.get(core(1)), None);
        let mut both = fixture.sensors(&topology(), &[core(0), core(1)], now);
        let readings = both.read(now);
        assert_eq!(readings.core_mhz.get(core(1)), Some(3999));
    }

    #[test]
    fn negative_or_unparsable_frequencies_are_omitted() {
        let fixture = Fixture::new();
        fixture.write("cpu/cpu0/cpufreq/scaling_cur_freq", "-5\n");
        fixture.write("cpu/cpu1/cpufreq/scaling_cur_freq", "fast\n");
        let now = Instant::now();
        let mut sensors = fixture.sensors(&topology(), &[core(0), core(1)], now);
        assert!(sensors.read(now).core_mhz.is_empty());
    }

    #[test]
    fn package_power_is_energy_delta_over_elapsed_time() {
        let fixture = Fixture::new();
        fixture.write("powercap/intel-rapl:0/energy_uj", "1000000\n");
        let start = Instant::now();
        let mut sensors = fixture.sensors(&[], &[], start);
        fixture.write("powercap/intel-rapl:0/energy_uj", "11000000\n");
        let readings = sensors.read(start + Duration::from_secs(1));
        assert_eq!(readings.package_power_w, Some(10.0));
    }

    #[test]
    fn a_counter_wrap_uses_the_range_only_when_it_exceeds_the_previous_reading() {
        let fixture = Fixture::new();
        fixture.write("powercap/intel-rapl:0/energy_uj", "9000000\n");
        fixture.write("powercap/intel-rapl:0/max_energy_range_uj", "10000000\n");
        let start = Instant::now();
        let mut sensors = fixture.sensors(&[], &[], start);
        fixture.write("powercap/intel-rapl:0/energy_uj", "1000000\n");
        assert_eq!(
            sensors.read(start + Duration::from_secs(1)).package_power_w,
            Some(2.0)
        );
        fixture.write("powercap/intel-rapl:0/max_energy_range_uj", "500000\n");
        fixture.write("powercap/intel-rapl:0/energy_uj", "100000\n");
        assert_eq!(
            sensors.read(start + Duration::from_secs(2)).package_power_w,
            None
        );
    }

    #[test]
    fn a_missing_reading_resets_the_baseline_so_the_next_good_one_omits_power() {
        let fixture = Fixture::new();
        fixture.write("powercap/intel-rapl:0/energy_uj", "1000000\n");
        let start = Instant::now();
        let mut sensors = fixture.sensors(&[], &[], start);
        togi_journal::fs::Fs::remove_file(
            fixture.fs.as_ref(),
            Path::new("/sensors/powercap/intel-rapl:0/energy_uj"),
        )
        .unwrap();
        assert_eq!(
            sensors.read(start + Duration::from_secs(1)).package_power_w,
            None
        );
        fixture.write("powercap/intel-rapl:0/energy_uj", "5000000\n");
        assert_eq!(
            sensors.read(start + Duration::from_secs(2)).package_power_w,
            None
        );
        fixture.write("powercap/intel-rapl:0/energy_uj", "7000000\n");
        assert_eq!(
            sensors.read(start + Duration::from_secs(3)).package_power_w,
            Some(2.0)
        );
    }
}
