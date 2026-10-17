use std::num::{NonZeroU16, NonZeroU32};
use std::path::{Path, PathBuf};

use togi_config::Config;
use togi_journal::ConfigLoaded;
use togi_machine::{Backend, Regime};

use crate::{FoldError, Secs};

/// What the tuner reads from the operator configuration: the trial durations, the checking
/// cycle, the pass count and the backend packages.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TunerConfig {
    pub(crate) search_trial: Secs,
    pub(crate) short_trial: Secs,
    pub(crate) checking_trial: Secs,
    pub(crate) checking_idle: Secs,
    pub(crate) checking_all_core: Secs,
    pub(crate) cycle: Box<[Regime]>,
    pub(crate) needed: NonZeroU16,
    pub(crate) miss: f64,
    pub(crate) rate: f64,
    pub(crate) mprime: Option<PathBuf>,
    pub(crate) ycruncher: Option<PathBuf>,
}

impl TunerConfig {
    /// What a `config.loaded` event stands for, or why its recorded configuration is not one this build accepts.
    pub(crate) fn from_loaded(loaded: &ConfigLoaded) -> Result<Self, FoldError> {
        let config = Config::from_snapshot(&loaded.config)?;
        Ok(Self::from_config(&config))
    }

    /// The configuration in force before any `config.loaded`: the defaults.
    pub(crate) fn initial() -> Self {
        Self::from_config(&Config::default())
    }

    pub(crate) fn from_config(config: &Config) -> Self {
        let secs = |value: u32| NonZeroU32::new(value).map_or(Secs::MIN, Secs::new);
        Self {
            search_trial: secs(config.search_trial().get()),
            short_trial: secs(config.short_trial().get()),
            checking_trial: secs(config.checking_trial().get()),
            checking_idle: secs(config.checking_idle().get()),
            checking_all_core: secs(config.checking_all_core().get()),
            cycle: config.checking_cycle().as_slice().into(),
            needed: u16::try_from(config.evidence().trials().get())
                .ok()
                .and_then(NonZeroU16::new)
                .unwrap_or(NonZeroU16::MIN),
            miss: config.evidence().miss().get(),
            rate: config.evidence().rate().get(),
            mprime: config
                .backends()
                .path(Backend::Mprime)
                .map(Path::to_path_buf),
            ycruncher: config
                .backends()
                .path(Backend::Ycruncher)
                .map(Path::to_path_buf),
        }
    }

    /// Whether `other` sets the same durations a checking schedule is built from.
    pub(crate) fn same_schedule(&self, other: &Self) -> bool {
        self.short_trial == other.short_trial
            && self.checking_trial == other.checking_trial
            && self.checking_idle == other.checking_idle
            && self.checking_all_core == other.checking_all_core
    }

    pub(crate) fn backend_path(&self, backend: Backend) -> Option<&Path> {
        match backend {
            Backend::Mprime => self.mprime.as_deref(),
            Backend::Ycruncher => self.ycruncher.as_deref(),
        }
    }
}
