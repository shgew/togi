use std::{collections::BTreeMap, fs, path::Path, path::PathBuf};

use serde::Deserialize;
use togi_journal::{
    ConfigBackends, ConfigChecking, ConfigDeadEnds, ConfigDurations, ConfigEvidence, ConfigSnapshot,
};
use togi_machine::{CoreId, Regime, serde_object::Object};

use crate::{
    error::{ConfigError, LoadError},
    overrides::{self, InitialOverride, RawCoreMap},
    values::{
        Backends, CheckingSchedule, DeadEndPolicy, DeadEndThreshold, EvidencePolicy, Probability,
        R7BudgetSeconds, TrialSeconds,
    },
};

pub const DEFAULT_PATH: &str = "/etc/togi/config.json";

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "Object<RawConfig>")]
pub struct Config {
    overrides: BTreeMap<CoreId, InitialOverride>,
    search_trial: TrialSeconds,
    short_trial: TrialSeconds,
    checking_trial: TrialSeconds,
    checking_idle: TrialSeconds,
    checking_all_core: R7BudgetSeconds,
    evidence: EvidencePolicy,
    checking_cycle: CheckingSchedule,
    dead_ends: DeadEndPolicy,
    backends: Backends,
    backend_user: Option<String>,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        Self::try_from(serde_json::from_str::<Object<RawConfig>>(text)?)
    }

    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let text = fs::read_to_string(path).map_err(|source| LoadError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text).map_err(|source| LoadError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    /// The journal's record of this configuration: every effective setting, owned by `togi-journal`.
    pub fn snapshot(&self) -> ConfigSnapshot {
        ConfigSnapshot {
            start_offsets: overrides::table(&self.overrides, overrides::start_only),
            candidate_solo_limits: overrides::table(&self.overrides, overrides::limit_only),
            durations: ConfigDurations {
                search_trial_s: self.search_trial.get(),
                short_trial_s: self.short_trial.get(),
                checking_trial_s: self.checking_trial.get(),
                checking_idle_s: self.checking_idle.get(),
                checking_all_core_s: self.checking_all_core.get(),
            },
            evidence: ConfigEvidence {
                miss: self.evidence.miss().get(),
                rate: self.evidence.rate().get(),
            },
            checking: ConfigChecking {
                cycle: self.checking_cycle.as_slice().to_vec(),
            },
            dead_ends: ConfigDeadEnds {
                inconclusive_in_a_row: self.dead_ends.inconclusive_in_a_row().get(),
                stray_crashes_in_a_row: self.dead_ends.stray_crashes_in_a_row().get(),
            },
            backends: ConfigBackends {
                mprime: self.backends.mprime().map(Path::to_path_buf),
                ycruncher: self.backends.ycruncher().map(Path::to_path_buf),
            },
            backend_user: self.backend_user.clone(),
        }
    }

    /// The configuration a journal recorded, validated again as a configuration file is: a snapshot that this
    /// build would refuse as a file is an error here too.
    pub fn from_snapshot(snapshot: &ConfigSnapshot) -> Result<Self, ConfigError> {
        Self::try_from(Object(RawConfig::from(snapshot)))
    }

    pub fn override_for(&self, core: CoreId) -> Option<InitialOverride> {
        self.overrides.get(&core).copied()
    }

    pub fn overrides(&self) -> impl Iterator<Item = (CoreId, InitialOverride)> + '_ {
        self.overrides.iter().map(|(core, value)| (*core, *value))
    }

    pub const fn search_trial(&self) -> TrialSeconds {
        self.search_trial
    }

    pub const fn short_trial(&self) -> TrialSeconds {
        self.short_trial
    }

    pub const fn checking_trial(&self) -> TrialSeconds {
        self.checking_trial
    }

    pub const fn checking_idle(&self) -> TrialSeconds {
        self.checking_idle
    }

    pub const fn checking_all_core(&self) -> R7BudgetSeconds {
        self.checking_all_core
    }

    pub const fn evidence(&self) -> EvidencePolicy {
        self.evidence
    }

    pub fn checking_cycle(&self) -> &CheckingSchedule {
        &self.checking_cycle
    }

    pub const fn dead_ends(&self) -> DeadEndPolicy {
        self.dead_ends
    }

    pub fn backends(&self) -> &Backends {
        &self.backends
    }

    pub fn backend_user(&self) -> Option<&str> {
        self.backend_user.as_deref()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            overrides: BTreeMap::new(),
            search_trial: TrialSeconds::literal(90),
            short_trial: TrialSeconds::literal(120),
            checking_trial: TrialSeconds::literal(120),
            checking_idle: TrialSeconds::literal(900),
            checking_all_core: R7BudgetSeconds::literal(1200),
            evidence: EvidencePolicy::default(),
            checking_cycle: CheckingSchedule::default(),
            dead_ends: DeadEndPolicy::default(),
            backends: Backends::default(),
            backend_user: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawConfig {
    start_offsets: Option<RawCoreMap>,
    candidate_solo_limits: Option<RawCoreMap>,
    durations: Option<Object<RawDurations>>,
    evidence: Option<Object<RawEvidence>>,
    checking: Option<Object<RawChecking>>,
    dead_ends: Option<Object<RawDeadEnds>>,
    backends: Option<Object<RawBackends>>,
    backend_user: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_field_names,
    reason = "the field names are the wire keys"
)]
struct RawDurations {
    search_trial_s: Option<i64>,
    short_trial_s: Option<i64>,
    checking_trial_s: Option<i64>,
    checking_idle_s: Option<i64>,
    checking_all_core_s: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvidence {
    miss: Option<f64>,
    rate: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawChecking {
    cycle: Option<Vec<Regime>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeadEnds {
    inconclusive_in_a_row: Option<i64>,
    stray_crashes_in_a_row: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBackends {
    mprime: Option<PathBuf>,
    ycruncher: Option<PathBuf>,
}

fn seconds(
    field: &'static str,
    value: Option<i64>,
    default: TrialSeconds,
) -> Result<TrialSeconds, ConfigError> {
    value.map_or(Ok(default), |value| {
        TrialSeconds::new(value).map_err(|source| ConfigError::Duration { field, source })
    })
}

fn threshold(
    field: &'static str,
    value: Option<i64>,
    default: DeadEndThreshold,
) -> Result<DeadEndThreshold, ConfigError> {
    value.map_or(Ok(default), |value| {
        DeadEndThreshold::new(value).map_err(|source| ConfigError::DeadEnd { field, source })
    })
}

fn probability(
    field: &'static str,
    value: Option<f64>,
    default: Probability,
) -> Result<Probability, ConfigError> {
    value.map_or(Ok(default), |value| {
        Probability::new(value).map_err(|source| ConfigError::Probability { field, source })
    })
}

impl TryFrom<Object<RawConfig>> for Config {
    type Error = ConfigError;

    fn try_from(Object(raw): Object<RawConfig>) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        let overrides = overrides::resolve(raw.start_offsets, raw.candidate_solo_limits)?;

        if let Some(Object(durations)) = raw.durations {
            config.search_trial = seconds(
                "search_trial_s",
                durations.search_trial_s,
                config.search_trial,
            )?;
            config.short_trial =
                seconds("short_trial_s", durations.short_trial_s, config.short_trial)?;
            config.checking_trial = seconds(
                "checking_trial_s",
                durations.checking_trial_s,
                config.checking_trial,
            )?;
            config.checking_idle = seconds(
                "checking_idle_s",
                durations.checking_idle_s,
                config.checking_idle,
            )?;
            if let Some(value) = durations.checking_all_core_s {
                config.checking_all_core =
                    R7BudgetSeconds::new(value).map_err(|source| ConfigError::Duration {
                        field: "checking_all_core_s",
                        source,
                    })?;
            }
        }

        if let Some(Object(evidence)) = raw.evidence {
            let defaults = config.evidence;
            let miss = probability("miss", evidence.miss, defaults.miss())?;
            let rate = probability("rate", evidence.rate, defaults.rate())?;
            config.evidence = EvidencePolicy::new(miss, rate)?;
        }

        if let Some(cycle) = raw.checking.and_then(|Object(checking)| checking.cycle) {
            config.checking_cycle = CheckingSchedule::new(cycle)?;
        }

        if let Some(Object(dead_ends)) = raw.dead_ends {
            let defaults = config.dead_ends;
            config.dead_ends = DeadEndPolicy::new(
                threshold(
                    "inconclusive_in_a_row",
                    dead_ends.inconclusive_in_a_row,
                    defaults.inconclusive_in_a_row(),
                )?,
                threshold(
                    "stray_crashes_in_a_row",
                    dead_ends.stray_crashes_in_a_row,
                    defaults.stray_crashes_in_a_row(),
                )?,
            );
        }

        if let Some(Object(backends)) = raw.backends {
            config.backends = Backends::new(backends.mprime, backends.ycruncher)?;
        }

        if raw.backend_user.as_deref() == Some("") {
            return Err(ConfigError::EmptyBackendUser);
        }
        config.backend_user = raw.backend_user;
        config.overrides = overrides;
        Ok(config)
    }
}

impl From<&ConfigSnapshot> for RawConfig {
    fn from(snapshot: &ConfigSnapshot) -> Self {
        let durations = &snapshot.durations;
        let seconds = |value: u32| Some(i64::from(value));
        Self {
            start_offsets: Some(overrides::raw_map(&snapshot.start_offsets)),
            candidate_solo_limits: Some(overrides::raw_map(&snapshot.candidate_solo_limits)),
            durations: Some(Object(RawDurations {
                search_trial_s: seconds(durations.search_trial_s),
                short_trial_s: seconds(durations.short_trial_s),
                checking_trial_s: seconds(durations.checking_trial_s),
                checking_idle_s: seconds(durations.checking_idle_s),
                checking_all_core_s: seconds(durations.checking_all_core_s),
            })),
            evidence: Some(Object(RawEvidence {
                miss: Some(snapshot.evidence.miss),
                rate: Some(snapshot.evidence.rate),
            })),
            checking: Some(Object(RawChecking {
                cycle: Some(snapshot.checking.cycle.clone()),
            })),
            dead_ends: Some(Object(RawDeadEnds {
                inconclusive_in_a_row: Some(i64::from(snapshot.dead_ends.inconclusive_in_a_row)),
                stray_crashes_in_a_row: Some(i64::from(snapshot.dead_ends.stray_crashes_in_a_row)),
            })),
            backends: Some(Object(RawBackends {
                mprime: snapshot.backends.mprime.clone(),
                ycruncher: snapshot.backends.ycruncher.clone(),
            })),
            backend_user: snapshot.backend_user.clone(),
        }
    }
}
