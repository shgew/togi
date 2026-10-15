use crate::Error;
use crate::files::{Files, first_stanza, read_trimmed};
use crate::sysfs::DRIVER_DIR;
use std::fmt;
use std::path::Path;

const GRANITE_RIDGE_CODENAME: &str = "23";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuSummary {
    pub family: u32,
    pub model: u32,
    pub name: String,
}

impl fmt::Display for CpuSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "family {:#X} model {:#X} {}",
            self.family, self.model, self.name
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DriverSummary {
    driver_version: String,
    smu_version: String,
}

impl fmt::Display for DriverSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "codename {GRANITE_RIDGE_CODENAME} (Granite Ridge), driver {}, SMU {}",
            self.driver_version, self.smu_version
        )
    }
}

/// Family 0x1A, model 0x40–0x4F, from the first `/proc/cpuinfo` stanza.
pub(crate) fn cpu(root: &Path, files: &Files) -> Result<CpuSummary, Error> {
    let stanza = first_stanza(files, &root.join("proc/cpuinfo"))?;
    let parse = |field: &'static str, text: &str| {
        text.parse::<u32>().map_err(|source| Error::ParseCpuInfo {
            field,
            text: text.to_owned(),
            source,
        })
    };
    let family_number = parse("family", stanza.get("cpu family"))?;
    let model_number = parse("model", stanza.get("model"))?;
    let summary = CpuSummary {
        family: family_number,
        model: model_number,
        name: stanza.get("model name").to_owned(),
    };
    if family_number == 0x1a && (0x40..=0x4f).contains(&model_number) {
        Ok(summary)
    } else {
        Err(Error::UnsupportedCpu { summary })
    }
}

pub(crate) fn driver(root: &Path, files: &Files, loaded: bool) -> Result<DriverSummary, Error> {
    if !loaded {
        return Err(Error::DriverNotLoaded);
    }
    let dir = root.join(DRIVER_DIR);
    let codename = read_trimmed(files, &dir.join("codename"))?;
    if codename != GRANITE_RIDGE_CODENAME {
        return Err(Error::Codename { found: codename });
    }
    Ok(DriverSummary {
        driver_version: read_trimmed(files, &dir.join("drv_version"))?,
        smu_version: read_trimmed(files, &dir.join("version"))?,
    })
}

/// The firmware and CPU strings of the BIOS context, all lossy UTF-8.
pub(crate) struct Firmware {
    pub(crate) bios_version: String,
    pub(crate) board: String,
    pub(crate) cpu_model: String,
    pub(crate) microcode: String,
}

pub(crate) fn firmware(root: &Path, files: &Files) -> Result<Firmware, Error> {
    let dmi = root.join("sys/class/dmi/id");
    let bios_version = read_trimmed(files, &dmi.join("bios_version"))?;
    let vendor = read_trimmed(files, &dmi.join("board_vendor"))?;
    let name = read_trimmed(files, &dmi.join("board_name"))?;
    let stanza = first_stanza(files, &root.join("proc/cpuinfo"))?;
    Ok(Firmware {
        bios_version,
        board: format!("{vendor} {name}"),
        cpu_model: stanza.get("model name").to_owned(),
        microcode: stanza.get("microcode").to_owned(),
    })
}
