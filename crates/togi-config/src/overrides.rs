use std::fmt;

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use togi_machine::{CoreId, PerCore, TargetOffset};

use crate::error::ConfigError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitialOverride {
    Start(TargetOffset),
    CandidateSoloLimit(TargetOffset),
}

impl InitialOverride {
    pub const fn offset(self) -> TargetOffset {
        match self {
            Self::Start(offset) | Self::CandidateSoloLimit(offset) => offset,
        }
    }
}

pub(crate) const START_OFFSETS: &str = "start_offsets";
pub(crate) const CANDIDATE_SOLO_LIMITS: &str = "candidate_solo_limits";

pub(crate) struct RawCoreMap(Vec<(String, i64)>);

impl<'de> Deserialize<'de> for RawCoreMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;

        impl<'de> Visitor<'de> for Entries {
            type Value = RawCoreMap;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object from core ids to integer offsets")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawCoreMap, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, i64>()? {
                    entries.push(entry);
                }
                Ok(RawCoreMap(entries))
            }
        }

        deserializer.deserialize_map(Entries)
    }
}

fn core_from_key(table: &'static str, key: &str) -> Result<CoreId, ConfigError> {
    let canonical = !key.is_empty()
        && key.bytes().all(|byte| byte.is_ascii_digit())
        && (key == "0" || !key.starts_with('0'));
    let value = key
        .parse::<i64>()
        .ok()
        .filter(|_| canonical)
        .ok_or_else(|| ConfigError::CoreKey {
            table,
            key: key.to_owned(),
        })?;
    CoreId::new(value).map_err(|source| ConfigError::Core { table, source })
}

fn offsets(
    table: &'static str,
    raw: Option<RawCoreMap>,
) -> Result<PerCore<Option<TargetOffset>>, ConfigError> {
    let mut resolved = PerCore::default();
    for (key, value) in raw.into_iter().flat_map(|raw| raw.0) {
        let core = core_from_key(table, &key)?;
        let offset = TargetOffset::new(value).map_err(|source| ConfigError::Offset {
            table,
            core,
            source,
        })?;
        if resolved.get(core).flatten().is_some() {
            return Err(ConfigError::DuplicateCore { table, core });
        }
        resolved.set(core, Some(offset));
    }
    Ok(resolved)
}

pub(crate) fn resolve(
    start_offsets: Option<RawCoreMap>,
    candidate_solo_limits: Option<RawCoreMap>,
) -> Result<PerCore<Option<InitialOverride>>, ConfigError> {
    let mut resolved: PerCore<Option<InitialOverride>> = offsets(START_OFFSETS, start_offsets)?
        .iter()
        .map(|(core, offset)| (core, offset.map(InitialOverride::Start)))
        .collect();
    for (core, offset) in offsets(CANDIDATE_SOLO_LIMITS, candidate_solo_limits)?.iter() {
        if resolved.get(core).flatten().is_some() {
            return Err(ConfigError::CoreConflict(core));
        }
        resolved.set(core, offset.map(InitialOverride::CandidateSoloLimit));
    }
    Ok(resolved)
}

pub(crate) fn table(
    overrides: &PerCore<Option<InitialOverride>>,
    select: fn(InitialOverride) -> Option<TargetOffset>,
) -> PerCore<TargetOffset> {
    overrides
        .iter()
        .filter_map(|(core, value)| Some((core, select(value?)?)))
        .collect()
}

pub(crate) fn raw_map(offsets: &PerCore<TargetOffset>) -> RawCoreMap {
    RawCoreMap(
        offsets
            .iter()
            .map(|(core, offset)| (core.get().to_string(), offset.get()))
            .collect(),
    )
}

pub(crate) const fn start_only(value: InitialOverride) -> Option<TargetOffset> {
    match value {
        InitialOverride::Start(offset) => Some(offset),
        InitialOverride::CandidateSoloLimit(_) => None,
    }
}

pub(crate) const fn limit_only(value: InitialOverride) -> Option<TargetOffset> {
    match value {
        InitialOverride::CandidateSoloLimit(offset) => Some(offset),
        InitialOverride::Start(_) => None,
    }
}
