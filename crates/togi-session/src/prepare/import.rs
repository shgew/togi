//! A versioned successor manifest of original observations and irreversible campaign history.
//!
//! The manifest is a pure function of the archived journals' bytes, so a startup that was killed before it committed
//! rebuilds the same one and appends only the identities the live journal lacks.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::value::RawValue;
use togi_journal::{
    Build, CarriedSource, DayImported, Event, EventKind, ImportClass, ImportCounts,
    ImportedRecordRef, MissingFact, ObservedProfile, Payload, Provenance, Seq, SessionId,
    SessionImported,
};
use togi_machine::{
    BiosContext, BootId, CcdProfile, CoreInfo, ObservedOffset, PerCore, TargetOffset,
};

/// The versioned successor contract; no predecessor conclusion grants permission.
pub(crate) const REASON: &str = "successor import v1; verified stock; no carried permission";

/// A day record kind and how an import classes it.
const DAY_KINDS: [(&str, ImportClass); 8] = [
    ("day.boot_start", ImportClass::Boot),
    ("day.checkpoint", ImportClass::Checkpoint),
    ("day.offsets_changed", ImportClass::ProfileChange),
    ("day.clean_stop", ImportClass::End),
    ("day.unclean_end", ImportClass::Fault),
    ("day.answer", ImportClass::Answer),
    ("day.record", ImportClass::Manual),
    ("day.fault", ImportClass::Fault),
];

/// Campaign facts cross reset boundaries; raw tuning observations do not.
const HISTORY_KINDS: &[&str] = &[
    "session.start",
    "day.applied", "day.step", "day.event", "day.veto", "climb.decision",
    "unit.mapping", "unit.stop", "campaign.start", "campaign.charge",
    "campaign.reclassified", "successor.imported",
];

const IMPORTED_KIND: &str = "day.imported";
const START_KIND: &str = "session.start";
const MCE_KIND: &str = "mce";
const TRIAL_PREFIX: &str = "trial.";
const DAY_PREFIX: &str = "day.";
const UNREADABLE: &str = "unreadable";

/// What a transition imports, and what it names without importing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Manifest {
    sources: Vec<CarriedSource>,
    records: Vec<DayImported>,
    duplicates: u64,
    unknown: Vec<ImportedRecordRef>,
    excluded: Vec<ImportedRecordRef>,
    campaign_records: Vec<DayImported>,
    topologies: BTreeMap<SessionId, Vec<CoreInfo>>,
}

impl Manifest {
    /// The records to append, oldest original identity first.
    pub fn records(&self) -> &[DayImported] {
        &self.records
    }

    pub fn sources(&self) -> &[CarriedSource] {
        &self.sources
    }

    pub fn reconstruction(&self) -> togi_tuner::policy::successor::Reconstruction {
        togi_tuner::policy::successor::reconstruct(&self.campaign_records, &self.unknown)
    }

    pub(crate) fn topology(&self, session: SessionId) -> Option<&[CoreInfo]> {
        self.topologies.get(&session).map(Vec::as_slice)
    }

    /// The identity of every record the import selects.
    pub fn selected(&self) -> impl ExactSizeIterator<Item = Provenance> + '_ {
        self.records.iter().map(|record| record.original)
    }

    /// Source records whose kind or payload this build cannot decode.
    pub fn unknown(&self) -> &[ImportedRecordRef] {
        &self.unknown
    }

    /// Source records that look like day observations but are not raw day records of a day boot.
    pub fn excluded(&self) -> &[ImportedRecordRef] {
        &self.excluded
    }

    /// The `session.imported` that commits the import once its `day.imported` events are durable.
    pub fn commitment(&self) -> SessionImported {
        let selected: Vec<Provenance> = self.selected().collect();
        let count = |len: usize| u64::try_from(len).unwrap_or(u64::MAX);
        SessionImported {
            sources: self.sources.clone(),
            counts: ImportCounts {
                selected: count(selected.len()),
                imported: count(self.records.len()),
                duplicates: self.duplicates,
                unknown: count(self.unknown.len()),
                excluded: count(self.excluded.len()),
            },
            selected,
            unknown: self.unknown.clone(),
            excluded: self.excluded.clone(),
            reason: REASON.to_owned(),
        }
    }
}

/// One archived journal's bytes.
pub(crate) struct Source {
    pub(crate) session: SessionId,
    pub(crate) bytes: Vec<u8>,
    pub(crate) carry_raw: bool,
}


/// The manifest of `sources`, newest archive first: each original identity is kept once, from the newest archive
/// that holds it.
pub(crate) fn manifest(sources: &[Source]) -> Manifest {
    let mut manifest = Manifest::default();
    let mut seen = HashSet::new();
    for source in sources {
        read(source, &mut manifest, &mut seen);
    }
    manifest.campaign_records.clone_from(&manifest.records);
    manifest.records.retain(|record| {
        HISTORY_KINDS.contains(&record.original_kind.as_str())
            || sources.iter().find(|source| source.session == record.original.session)
                .is_none_or(|source| source.carry_raw)
    });
    manifest.records.sort_by_key(|record| (record.original.session, record.original.seq));
    manifest.sources.sort_by_key(|source| source.session);
    manifest
}

/// The fields of a journal line this reader needs whatever the line's schema.
#[derive(Deserialize)]
struct Light {
    seq: Seq,
    time: Timestamp,
    boot: BootId,
    kind: String,
    data: Box<RawValue>,
}

/// The build numbers of a `session.start`, whatever else its payload holds.
#[derive(Deserialize)]
struct StartNumbers {
    build: BuildNumbers,
}

#[derive(Deserialize)]
struct BuildNumbers {
    schema: u32,
    ruleset: u32,
}

/// The nonblank newline-terminated lines of a journal with their 1-based line numbers; `None` for bytes that are
/// not UTF-8. A torn tail, the bytes after the last newline, is never a line.
fn lines(bytes: &[u8]) -> impl Iterator<Item = (u64, Option<&str>)> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .enumerate()
        .filter_map(|(index, line)| Some((index, line.strip_suffix(b"\n")?)))
        .filter(|(_, line)| !line.is_empty())
        .map(|(index, line)| {
            (
                u64::try_from(index).map_or(u64::MAX, |index| index.saturating_add(1)),
                std::str::from_utf8(line).ok(),
            )
        })
}

fn read(source: &Source, manifest: &mut Manifest, seen: &mut HashSet<Provenance>) {
    let parsed: Vec<(u64, Option<(Light, &str)>)> = lines(&source.bytes)
        .map(|(number, line)| {
            let light = line.and_then(|text| {
                serde_json::from_str::<Light>(text)
                    .ok()
                    .map(|light| (light, text))
            });
            (number, light)
        })
        .collect();
    let topology = start_of(&parsed, &mut manifest.sources, source.session);
    if topology.is_none() {
        manifest.unknown.push(ImportedRecordRef {
            original: Provenance { session: source.session, seq: Seq::FIRST },
            kind: START_KIND.to_owned(),
        });
    }
    if let Some(topology) = &topology {
        manifest.topologies.insert(source.session, topology.clone());
    }
    let (day_boots, trial_boots) = boot_roles(&parsed);
    for (number, light) in &parsed {
        let Some((light, text)) = light else {
            manifest.unknown.push(ImportedRecordRef {
                original: Provenance {
                    session: source.session,
                    seq: Seq::new(*number).unwrap_or(Seq::FIRST),
                },
                kind: UNREADABLE.to_owned(),
            });
            continue;
        };
        let original = Provenance {
            session: source.session,
            seq: light.seq,
        };
        match classify(light, text, &day_boots, &trial_boots) {
            Class::Ignored => {}
            Class::Unknown => manifest.unknown.push(ImportedRecordRef {
                original,
                kind: light.kind.clone(),
            }),
            Class::Excluded => manifest.excluded.push(ImportedRecordRef {
                original,
                kind: light.kind.clone(),
            }),
            Class::Copy(record) => keep(*record, manifest, seen),
            Class::Day { class, event } => {
                let record = day_record(original, light, class, &event, topology.as_deref());
                keep(record, manifest, seen);
            }
        }
    }
}

/// Records the source of an archive whose first complete line is its `session.start`, and returns that session's
/// topology when it decodes.
fn start_of(
    parsed: &[(u64, Option<(Light, &str)>)],
    sources: &mut Vec<CarriedSource>,
    session: SessionId,
) -> Option<Vec<CoreInfo>> {
    let (light, text) = parsed.iter().find_map(|(_, line)| line.as_ref())?;
    if light.kind != START_KIND {
        return None;
    }
    if let Ok(start) = serde_json::from_str::<StartNumbers>(light.data.get()) {
        sources.push(CarriedSource {
            session,
            schema: start.build.schema,
            ruleset: start.build.ruleset,
        });
    }
    let Ok(Event {
        payload: Payload::SessionStart(start),
        ..
    }) = Event::decode(text)
    else {
        return None;
    };
    Some(start.cores)
}

/// The boots that hold a `day.boot_start` and the boots that hold a trial event.
fn boot_roles(parsed: &[(u64, Option<(Light, &str)>)]) -> (BTreeSet<BootId>, BTreeSet<BootId>) {
    let mut day = BTreeSet::new();
    let mut trial = BTreeSet::new();
    for (light, _) in parsed.iter().filter_map(|(_, line)| line.as_ref()) {
        if light.kind == "day.boot_start" {
            day.insert(light.boot.clone());
        } else if light.kind.starts_with(TRIAL_PREFIX) {
            trial.insert(light.boot.clone());
        }
    }
    (day, trial)
}

enum Class {
    /// Not a day observation.
    Ignored,
    /// A day observation this build cannot decode.
    Unknown,
    /// A record that resembles a day observation but is not a raw one of a day boot.
    Excluded,
    /// An earlier import's `day.imported`, carried on with its original provenance.
    Copy(Box<DayImported>),
    Day {
        class: ImportClass,
        event: Box<Event>,
    },
}

fn classify(
    light: &Light,
    text: &str,
    day_boots: &BTreeSet<BootId>,
    trial_boots: &BTreeSet<BootId>,
) -> Class {
    if light.kind.parse::<EventKind>().is_err() {
        return Class::Unknown;
    }
    if light.kind == MCE_KIND {
        return classify_mce(light, text, day_boots, trial_boots);
    }
    if !light.kind.starts_with(DAY_PREFIX)
        && !HISTORY_KINDS.contains(&light.kind.as_str())
        && light.kind != "exposure.observed"
    {
        return Class::Ignored;
    }
    let Ok(event) = Event::decode(text) else {
        return Class::Unknown;
    };
    if light.kind == IMPORTED_KIND {
        let Payload::DayImported(record) = event.payload else {
            return Class::Unknown;
        };
        return Class::Copy(Box::new(record));
    }
    if HISTORY_KINDS.contains(&light.kind.as_str()) || light.kind == "exposure.observed" {
        return Class::Day { class: ImportClass::Manual, event: Box::new(event) };
    }
    match DAY_KINDS.iter().find(|(kind, _)| *kind == light.kind) {
        Some((_, class)) => Class::Day {
            class: *class,
            event: Box::new(event),
        },
        None => Class::Ignored,
    }
}

/// A machine check is a day observation only on a day boot that ran no trial and was reported with none.
fn classify_mce(
    light: &Light,
    text: &str,
    day_boots: &BTreeSet<BootId>,
    trial_boots: &BTreeSet<BootId>,
) -> Class {
    let Ok(event) = Event::decode(text) else {
        return Class::Unknown;
    };
    let Payload::Mce(mce) = &event.payload else {
        return Class::Excluded;
    };
    if day_boots.contains(&light.boot) && !trial_boots.contains(&light.boot) && mce.trial.is_none()
    {
        Class::Day {
            class: ImportClass::Fault,
            event: Box::new(event),
        }
    } else {
        Class::Excluded
    }
}

fn day_record(
    original: Provenance,
    light: &Light,
    class: ImportClass,
    event: &Event,
    topology: Option<&[CoreInfo]>,
) -> DayImported {
    let original = if let Payload::ExposureObserved(observed) = &event.payload {
        Provenance { session: observed.source.session, seq: observed.source.seq }
    } else {
        original
    };
    let facts = Facts::of(&event.payload);
    let Facts {
        build,
        firmware,
        offsets,
    } = facts;
    let profile = offsets.map(|offsets| profile(offsets, topology));
    let mut missing = Vec::new();
    if build.is_none() {
        missing.push(MissingFact::Build);
    }
    if firmware.is_none() {
        missing.push(MissingFact::Firmware);
    }
    if profile.is_none() {
        missing.push(MissingFact::Profile);
    }
    DayImported {
        original,
        original_kind: event.payload.kind(),
        raw: light.data.get().to_owned(),
        time: light.time,
        boot: light.boot.clone(),
        build,
        firmware,
        profile,
        class,
        missing,
    }
}

/// What a day payload records about its build, firmware and offsets; each is absent when the kind does not carry it.
struct Facts<'a> {
    build: Option<Build>,
    firmware: Option<BiosContext>,
    offsets: Option<&'a PerCore<ObservedOffset>>,
}

impl<'a> Facts<'a> {
    fn of(payload: &'a Payload) -> Self {
        let none = Self {
            build: None,
            firmware: None,
            offsets: None,
        };
        if let Payload::DayBootStart(start) = payload {
            return Self {
                build: Some(start.build.clone()),
                firmware: Some(start.bios.clone()),
                offsets: Some(&start.offsets),
            };
        }
        let offsets = if let Payload::DayOffsetsChanged(changed) = payload {
            Some(&changed.offsets)
        } else if let Payload::DayUncleanEnd(end) = payload {
            Some(&end.offsets)
        } else if let Payload::DayAnswer(answer) = payload {
            Some(&answer.offsets)
        } else if let Payload::DayRecord(record) = payload {
            Some(record.offsets.get())
        } else if let Payload::DayFault(fault) = payload {
            Some(&fault.offsets)
        } else {
            None
        };
        Self { offsets, ..none }
    }
}

/// The CCD profile of `offsets` when they are valid targets that are uniform within each CCD of `topology`;
/// otherwise their per-core shape, retained as observed.
fn profile(offsets: &PerCore<ObservedOffset>, topology: Option<&[CoreInfo]>) -> ObservedProfile {
    let targets: Option<PerCore<TargetOffset>> = offsets
        .iter_ref()
        .map(|(core, offset)| {
            TargetOffset::new(offset.0)
                .ok()
                .map(|target| (core, target))
        })
        .collect::<Option<Vec<_>>>()
        .map(|pairs| pairs.into_iter().collect());
    let ccd = targets
        .zip(topology)
        .and_then(|(targets, topology)| CcdProfile::of_cores(&targets, topology));
    match ccd {
        Some(ccd) => ObservedProfile::Ccd {
            ccd,
            cores: *offsets,
        },
        None => ObservedProfile::PerCore { cores: *offsets },
    }
}

fn keep(record: DayImported, manifest: &mut Manifest, seen: &mut HashSet<Provenance>) {
    if seen.insert(record.original) {
        manifest.records.push(record);
    } else {
        manifest.duplicates += 1;
    }
}
