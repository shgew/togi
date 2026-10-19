use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use togi_machine::{CoreId, CoreInfo, PerCore};
use tokio::time::Instant;

use crate::error::SensorError;
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
    pub(crate) cores_power_w: Option<f64>,
}

/// Reads temperatures, all-core clocks and package and core-domain energy.
#[derive(Debug)]
pub(crate) struct Sensors {
    hwmon: PathBuf,
    powercap: PathBuf,
    clocks: Vec<(CoreId, PathBuf)>,
    energy: Option<i64>,
    cores_energy: Option<i64>,
    energy_at: Instant,
    inputs: Inputs,
}

fn read_integer(inputs: &Inputs, path: &Path) -> Result<Option<i64>, SensorError> {
    let Ok(text) = inputs.read_to_string(path) else {
        return Ok(None);
    };
    let text = text.trim();
    let digits = text.strip_prefix('-').or_else(|| text.strip_prefix('+')).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(None);
    }
    text.parse().map(Some).map_err(|source| SensorError {
        path: path.to_path_buf(),
        source,
    })
}

impl Sensors {
    /// Takes the energy baseline at `now`, when timing begins.
    pub(crate) fn with_inputs(
        roots: &Roots,
        topology: &[CoreInfo],
        now: Instant,
        inputs: Inputs,
    ) -> Result<Self, SensorError> {
        let clocks = topology
            .iter()
            .filter_map(|info| {
                let first = info.cpus.first()?;
                Some((
                    info.core,
                    roots
                        .cpu
                        .join(format!("cpu{}", first.get()))
                        .join("cpufreq/scaling_cur_freq"),
                ))
            })
            .collect();
        let powercap = roots.powercap.join("intel-rapl:0");
        Ok(Self {
            hwmon: roots.hwmon.clone(),
            energy: read_integer(&inputs, &powercap.join("energy_uj"))?,
            cores_energy: read_integer(&inputs, &powercap.join("intel-rapl:0:0/energy_uj"))?,
            powercap,
            clocks,
            energy_at: now,
            inputs,
        })
    }

    pub(crate) fn read(&mut self, now: Instant) -> Result<Readings, SensorError> {
        let (tctl_c, tccd_c) = temperatures(&self.inputs, &self.hwmon)?;
        let mut core_mhz = PerCore::default();
        for (core, path) in &self.clocks {
            if let Some(khz) = read_integer(&self.inputs, path)?.filter(|khz| *khz >= 0) {
                core_mhz.set(*core, khz / 1000);
            }
        }
        let energy = read_integer(&self.inputs, &self.powercap.join("energy_uj"))?;
        let cores_path = self.powercap.join("intel-rapl:0:0");
        let cores_energy = read_integer(&self.inputs, &cores_path.join("energy_uj"))?;
        let package_power_w = self.power(&self.powercap, energy, self.energy, now)?;
        let cores_power_w = self.power(&cores_path, cores_energy, self.cores_energy, now)?;
        self.energy = energy;
        self.cores_energy = cores_energy;
        self.energy_at = now;
        Ok(Readings {
            tctl_c,
            tccd_c,
            core_mhz,
            package_power_w,
            cores_power_w,
        })
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "microjoule deltas are far below 2^53"
    )]
    fn power(
        &self,
        domain: &Path,
        energy: Option<i64>,
        baseline: Option<i64>,
        now: Instant,
    ) -> Result<Option<f64>, SensorError> {
        let (Some(current), Some(previous)) = (energy, baseline) else {
            return Ok(None);
        };
        if current < 0 || previous < 0 {
            return Ok(None);
        }
        let Some(elapsed) = now.checked_duration_since(self.energy_at) else {
            return Ok(None);
        };
        if elapsed.is_zero() {
            return Ok(None);
        }
        let mut delta = current - previous;
        if delta < 0 {
            let Some(limit) = read_integer(&self.inputs, &domain.join("max_energy_range_uj"))? else {
                return Ok(None);
            };
            if limit <= previous {
                return Ok(None);
            }
            delta += limit;
        }
        Ok((delta >= 0).then(|| delta as f64 / 1e6 / elapsed.as_secs_f64()))
    }
}

/// The first hwmon directory named `k10temp` or `zenpower`; it is not skipped for lacking sensors.
fn temperatures(inputs: &Inputs, root: &Path) -> Result<(Option<i64>, Option<BTreeMap<String, i64>>), SensorError> {
    let Ok(entries) = inputs.read_dir(root) else {
        return Ok((None, None));
    };
    let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    dirs.sort();
    for dir in dirs {
        let name = inputs.read_to_string(&dir.join("name")).unwrap_or_default();
        if !matches!(name.trim(), "k10temp" | "zenpower") {
            continue;
        }
        let Ok(files) = inputs.read_dir(&dir) else {
            return Ok((None, None));
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
            let Some(millidegrees) = read_integer(inputs, &input)? else {
                continue;
            };
            if label == "Tctl" {
                tctl = Some(millidegrees / 1000);
            } else {
                tccd.insert(label.to_owned(), millidegrees / 1000);
            }
        }
        return Ok((tctl, (!tccd.is_empty()).then_some(tccd)));
    }
    Ok((None, None))
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

        fn sensors(&self, topology: &[CoreInfo], now: Instant) -> Sensors {
            Sensors::with_inputs(&Self::roots(), topology, now, self.inputs()).unwrap()
        }
    }

    fn core(n: u8) -> CoreId {
        CoreId::new(i64::from(n)).unwrap()
    }

    fn topology() -> Vec<CoreInfo> {
        vec![
            CoreInfo {
                core: core(0),
                ccd: CcdId::new(0).unwrap(),
                cpus: vec![CpuId::from(0), CpuId::from(8)],
            },
            CoreInfo {
                core: core(1),
                ccd: CcdId::new(0).unwrap(),
                cpus: vec![CpuId::from(1), CpuId::from(9)],
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
        let (tctl, tccd) = temperatures(&fixture.inputs(), &Fixture::roots().hwmon).unwrap();
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
            temperatures(&fixture.inputs(), &Fixture::roots().hwmon).unwrap(),
            (None, None)
        );
        assert_eq!(
            temperatures(&fixture.inputs(), Path::new("/sensors/absent")).unwrap(),
            (None, None)
        );
    }

    #[test_case::test_case(None, None, None, None; "missing domain")]
    #[test_case::test_case(Some("1000000"), Some("4000000"), None, Some(3.0); "delta")]
    #[test_case::test_case(Some("9000000"), Some("1000000"), Some("10000000"), Some(2.0); "wrap")]
    #[test_case::test_case(Some("9000000"), Some("1000000"), None, None; "unexplained decrease")]
    #[test_case::test_case(Some("9000000"), Some("1000000"), Some("500000"), None; "invalid wrap range")]
    #[test_case::test_case(Some("1000000"), Some("invalid"), None, None; "invalid reading")]
    #[test_case::test_case(Some("1000000"), Some("-1"), None, None; "negative reading")]
    fn core_domain_uses_its_own_baseline_and_wrap_range(
        initial: Option<&str>,
        current: Option<&str>,
        range: Option<&str>,
        expected: Option<f64>,
    ) {
        let fixture = Fixture::new();
        fixture.write("powercap/intel-rapl:0/energy_uj", "100000000");
        if let Some(value) = initial {
            fixture.write("powercap/intel-rapl:0/intel-rapl:0:0/energy_uj", value);
        }
        if let Some(value) = range {
            fixture.write(
                "powercap/intel-rapl:0/intel-rapl:0:0/max_energy_range_uj",
                value,
            );
        }
        let start = Instant::now();
        let mut sensors = fixture.sensors(&[], start);
        if let Some(value) = current {
            fixture.write("powercap/intel-rapl:0/intel-rapl:0:0/energy_uj", value);
        }
        assert_eq!(
            sensors.read(start + Duration::from_secs(1)).unwrap().cores_power_w,
            expected
        );
    }

    #[test]
    fn clocks_come_from_the_first_cpu_of_every_core_including_idle_cores() {
        let fixture = Fixture::new();
        fixture.write("cpu/cpu0/cpufreq/scaling_cur_freq", "4500000\n");
        fixture.write("cpu/cpu1/cpufreq/scaling_cur_freq", "3999999\n");
        fixture.write("cpu/cpu8/cpufreq/scaling_cur_freq", "1000\n");
        let now = Instant::now();
        let mut sensors = fixture.sensors(&topology(), now);
        let readings = sensors.read(now + Duration::from_secs(1)).unwrap();
        assert_eq!(readings.core_mhz.get(core(0)), Some(4500));
        assert_eq!(readings.core_mhz.get(core(1)), Some(3999));
    }

    #[test_case::test_case(None; "missing")]
    #[test_case::test_case(Some("-5\n"); "negative")]
    #[test_case::test_case(Some("fast\n"); "unparsable")]
    fn missing_or_invalid_frequencies_are_omitted(value: Option<&str>) {
        let fixture = Fixture::new();
        if let Some(value) = value {
            fixture.write("cpu/cpu0/cpufreq/scaling_cur_freq", value);
        }
        let now = Instant::now();
        let mut sensors = fixture.sensors(&topology(), now);
        assert!(sensors.read(now).unwrap().core_mhz.is_empty());
    }

    #[test]
    fn package_power_is_energy_delta_over_elapsed_time() {
        let fixture = Fixture::new();
        fixture.write("powercap/intel-rapl:0/energy_uj", "1000000\n");
        let start = Instant::now();
        let mut sensors = fixture.sensors(&[], start);
        fixture.write("powercap/intel-rapl:0/energy_uj", "11000000\n");
        let readings = sensors.read(start + Duration::from_secs(1)).unwrap();
        assert_eq!(readings.package_power_w, Some(10.0));
    }

    #[test]
    fn a_counter_wrap_uses_the_range_only_when_it_exceeds_the_previous_reading() {
        let fixture = Fixture::new();
        fixture.write("powercap/intel-rapl:0/energy_uj", "9000000\n");
        fixture.write("powercap/intel-rapl:0/max_energy_range_uj", "10000000\n");
        let start = Instant::now();
        let mut sensors = fixture.sensors(&[], start);
        fixture.write("powercap/intel-rapl:0/energy_uj", "1000000\n");
        assert_eq!(
            sensors.read(start + Duration::from_secs(1)).unwrap().package_power_w,
            Some(2.0)
        );
        fixture.write("powercap/intel-rapl:0/max_energy_range_uj", "500000\n");
        fixture.write("powercap/intel-rapl:0/energy_uj", "100000\n");
        assert_eq!(
            sensors.read(start + Duration::from_secs(2)).unwrap().package_power_w,
            None
        );
    }

    #[test_case::test_case("intel-rapl:0", false; "package")]
    #[test_case::test_case("intel-rapl:0/intel-rapl:0:0", true; "cores")]
    fn a_missing_reading_resets_the_baseline_so_the_next_good_one_omits_power(
        domain: &str,
        cores: bool,
    ) {
        let fixture = Fixture::new();
        let path = format!("powercap/{domain}/energy_uj");
        fixture.write(&path, "1000000\n");
        let start = Instant::now();
        let mut sensors = fixture.sensors(&[], start);
        let power = |readings: Readings| {
            if cores {
                readings.cores_power_w
            } else {
                readings.package_power_w
            }
        };
        togi_journal::fs::Fs::remove_file(fixture.fs.as_ref(), &Path::new("/sensors").join(&path))
            .unwrap();
        assert_eq!(power(sensors.read(start + Duration::from_secs(1)).unwrap()), None);
        fixture.write(&path, "5000000\n");
        assert_eq!(power(sensors.read(start + Duration::from_secs(2)).unwrap()), None);
        fixture.write(&path, "7000000\n");
        assert_eq!(
            power(sensors.read(start + Duration::from_secs(3)).unwrap()),
            Some(2.0)
        );
    }

    #[test_case::test_case("9223372036854775807", Some(i64::MAX); "largest integer")]
    #[test_case::test_case("-9223372036854775808", Some(i64::MIN); "smallest integer")]
    #[test_case::test_case("unknown", None; "unrecognized optional sensor")]
    #[test_case::test_case("922337203685477580800unknown", None; "overflowing prefix is not an integer")]
    #[test_case::test_case("-", None; "sign without digits")]
    #[test_case::test_case("+1", Some(1); "explicit positive sign")]
    fn recognized_sensor_values_are_exact(text: &str, expected: Option<i64>) {
        let fixture = Fixture::new();
        fixture.write("value", text);
        assert_eq!(
            read_integer(&fixture.inputs(), Path::new("/sensors/value")).unwrap(),
            expected
        );
    }

    #[test_case::test_case("9223372036854775808"; "positive overflow")]
    #[test_case::test_case("-9223372036854775809"; "negative overflow")]
    fn recognized_sensor_overflow_keeps_its_parse_error(text: &str) {
        let fixture = Fixture::new();
        fixture.write("value", text);
        let error = read_integer(&fixture.inputs(), Path::new("/sensors/value")).unwrap_err();
        assert_eq!(error.path, Path::new("/sensors/value"));
        assert!(std::error::Error::source(&error).unwrap().is::<std::num::ParseIntError>());
    }

    #[test_case::test_case("cpu/cpu0/cpufreq/scaling_cur_freq"; "frequency")]
    #[test_case::test_case("hwmon/hwmon0/temp1_input"; "temperature")]
    #[test_case::test_case("powercap/intel-rapl:0/energy_uj"; "package energy")]
    #[test_case::test_case("powercap/intel-rapl:0/intel-rapl:0:0/energy_uj"; "cores energy")]
    #[test_case::test_case("powercap/intel-rapl:0/max_energy_range_uj"; "package wrap")]
    #[test_case::test_case("powercap/intel-rapl:0/intel-rapl:0:0/max_energy_range_uj"; "cores wrap")]
    fn overflowing_sensor_observations_refuse_the_read(path: &str) {
        let fixture = Fixture::new();
        fixture.write("hwmon/hwmon0/name", "k10temp");
        fixture.write("hwmon/hwmon0/temp1_label", "Tctl");
        fixture.write("powercap/intel-rapl:0/energy_uj", "10");
        fixture.write("powercap/intel-rapl:0/intel-rapl:0:0/energy_uj", "10");
        let now = Instant::now();
        let mut sensors = fixture.sensors(&topology(), now);
        fixture.write("powercap/intel-rapl:0/energy_uj", "1");
        fixture.write("powercap/intel-rapl:0/intel-rapl:0:0/energy_uj", "1");
        fixture.write(path, "9223372036854775808");
        let error = sensors.read(now + Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.path, Path::new("/sensors").join(path));
    }

    #[test_case::test_case("powercap/intel-rapl:0/energy_uj"; "package baseline")]
    #[test_case::test_case("powercap/intel-rapl:0/intel-rapl:0:0/energy_uj"; "cores baseline")]
    fn overflowing_energy_baselines_refuse_initialization(path: &str) {
        let fixture = Fixture::new();
        fixture.write(path, "9223372036854775808");
        let error = Sensors::with_inputs(
            &Fixture::roots(), &[], Instant::now(), fixture.inputs()
        ).unwrap_err();
        assert_eq!(error.path, Path::new("/sensors").join(path));
    }
}
