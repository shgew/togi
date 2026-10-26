use std::{fs::File, io, path::Path, path::PathBuf};

use serde::{Deserialize, de::IgnoredAny};
use togi_journal::{ConfigBackends, ConfigDeadEnds, ConfigSnapshot};
use togi_machine::serde_object::Object;

use crate::{
    error::{ConfigError, LoadError, Retired},
    values::{Backends, DeadEndPolicy, DeadEndThreshold},
};

pub const DEFAULT_PATH: &str = "/etc/togi/config.json";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Config {
    dead_ends: DeadEndPolicy,
    backends: Backends,
    backend_user: Option<String>,
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        Self::try_from(Object::<RawConfig>::deserialize(deserializer)?)
            .map_err(|error| D::Error::custom(togi_machine::error_chain(&error)))
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        Self::try_from(serde_json::from_str::<Object<RawConfig>>(text)?)
    }

    pub fn load(path: &Path) -> Result<Self, LoadError> {
        Self::load_with(path, |path| File::open(path))
    }

    pub(crate) fn load_with<R: io::Read>(
        path: &Path,
        open: impl FnOnce(&Path) -> io::Result<R>,
    ) -> Result<Self, LoadError> {
        let read = || {
            let mut text = String::new();
            open(path)?.read_to_string(&mut text)?;
            Ok(text)
        };
        let text = read().map_err(|source| LoadError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text).map_err(|source| LoadError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    /// The journal's record of this configuration: every effective setting, owned by `togi-journal`. The
    /// retired Ruleset 12 settings are not written.
    pub fn snapshot(&self) -> ConfigSnapshot {
        ConfigSnapshot {
            start_offsets: None,
            candidate_solo_limits: None,
            durations: None,
            evidence: None,
            checking: None,
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
    /// build would refuse as a file is an error here too. The retired settings a historical snapshot carries
    /// are ignored: a session that ran with them is read, never configured with them again.
    pub fn from_snapshot(snapshot: &ConfigSnapshot) -> Result<Self, ConfigError> {
        Self::try_from(Object(RawConfig::from(snapshot)))
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

/// The wire shape. The settings Ruleset 13 retired parse only so that setting one names it in an error; their
/// values are never read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawConfig {
    start_offsets: Option<IgnoredAny>,
    candidate_solo_limits: Option<IgnoredAny>,
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
    search_trial_s: Option<IgnoredAny>,
    short_trial_s: Option<IgnoredAny>,
    checking_trial_s: Option<IgnoredAny>,
    checking_idle_s: Option<IgnoredAny>,
    checking_all_core_s: Option<IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvidence {
    miss: Option<IgnoredAny>,
    rate: Option<IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawChecking {
    cycle: Option<IgnoredAny>,
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

impl RawConfig {
    /// The first retired setting this configuration sets, in the order the specification lists them.
    fn retired(&self) -> Option<Retired> {
        let durations = self.durations.as_ref().map(|Object(durations)| durations);
        let evidence = self.evidence.as_ref().map(|Object(evidence)| evidence);
        let checking = self.checking.as_ref().map(|Object(checking)| checking);
        [
            (self.start_offsets.is_some(), Retired::StartOffsets),
            (
                self.candidate_solo_limits.is_some(),
                Retired::CandidateSoloLimits,
            ),
            (
                durations.is_some_and(|d| d.search_trial_s.is_some()),
                Retired::SearchTrial,
            ),
            (
                durations.is_some_and(|d| d.short_trial_s.is_some()),
                Retired::ShortTrial,
            ),
            (
                durations.is_some_and(|d| d.checking_trial_s.is_some()),
                Retired::CheckingTrial,
            ),
            (
                durations.is_some_and(|d| d.checking_idle_s.is_some()),
                Retired::CheckingIdle,
            ),
            (
                durations.is_some_and(|d| d.checking_all_core_s.is_some()),
                Retired::CheckingAllCore,
            ),
            (
                evidence.is_some_and(|e| e.miss.is_some()),
                Retired::EvidenceMiss,
            ),
            (
                evidence.is_some_and(|e| e.rate.is_some()),
                Retired::EvidenceRate,
            ),
            (
                checking.is_some_and(|c| c.cycle.is_some()),
                Retired::CheckingCycle,
            ),
        ]
        .into_iter()
        .find_map(|(set, key)| set.then_some(key))
    }
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

impl TryFrom<Object<RawConfig>> for Config {
    type Error = ConfigError;

    fn try_from(Object(raw): Object<RawConfig>) -> Result<Self, ConfigError> {
        if let Some(key) = raw.retired() {
            return Err(ConfigError::Retired(key));
        }
        let mut config = Self::default();

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
        Ok(config)
    }
}

impl From<&ConfigSnapshot> for RawConfig {
    fn from(snapshot: &ConfigSnapshot) -> Self {
        Self {
            start_offsets: None,
            candidate_solo_limits: None,
            durations: None,
            evidence: None,
            checking: None,
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
