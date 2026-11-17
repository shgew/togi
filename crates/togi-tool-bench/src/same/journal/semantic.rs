//! On-demand comparison after retirement of explicitly named base record kinds.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Difference, Disk, JournalError, Source, normalize};

mod derived;

type Sequences = BTreeMap<u64, Option<u64>>;
type Identities = BTreeMap<String, Sequences>;
type Journals = BTreeMap<String, Journal>;

struct Journal {
    path: PathBuf,
    session: String,
    records: Vec<Value>,
    derived: Vec<derived::Seeds>,
}

/// Counts compare the nth remaining record of each class, without hiding other classes.
#[derive(Clone, Debug, Default)]
pub struct ClassCounts {
    pub base: usize,
    pub head: usize,
    pub identical: usize,
    pub differing: usize,
    pub messages: usize,
    pub evidence_differing: usize,
}

/// Full-record equality includes messages; evidence equality excludes only `msg`.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub retired: BTreeMap<String, usize>,
    pub rebased_cursor_causes: usize,
    pub rebased_messages: BTreeMap<togi_journal::EventKind, usize>,
    pub classes: BTreeMap<String, ClassCounts>,
    pub declared_fields: BTreeMap<&'static str, usize>,
}

impl Summary {
    fn declare(&mut self, field: &'static str, changed: bool) {
        if changed {
            *self.declared_fields.entry(field).or_default() += 1;
        }
    }

    pub(crate) fn merge(&mut self, other: Self) {
        for (kind, count) in other.retired {
            *self.retired.entry(kind).or_default() += count;
        }
        self.rebased_cursor_causes += other.rebased_cursor_causes;
        for (kind, count) in other.rebased_messages {
            *self.rebased_messages.entry(kind).or_default() += count;
        }
        for (field, count) in other.declared_fields {
            *self.declared_fields.entry(field).or_default() += count;
        }
        for (kind, counts) in other.classes {
            let total = self.classes.entry(kind).or_default();
            total.base += counts.base;
            total.head += counts.head;
            total.identical += counts.identical;
            total.differing += counts.differing;
            total.messages += counts.messages;
            total.evidence_differing += counts.evidence_differing;
        }
    }

    pub fn write(&self, w: &mut impl io::Write) -> io::Result<()> {
        for (kind, count) in &self.retired {
            writeln!(
                w,
                "semantic: retired {kind}: base_dropped={count} head_dropped=0"
            )?;
        }
        writeln!(
            w,
            "semantic: rebased {} last-event cursor causes across retired records",
            self.rebased_cursor_causes
        )?;
        for (kind, count) in &self.rebased_messages {
            writeln!(
                w,
                "semantic: rebased {}.msg from typed payload references: differing_records={count}",
                kind.as_str()
            )?;
        }
        for (field, count) in &self.declared_fields {
            writeln!(w, "semantic: declared {field}: differing_records={count}")?;
        }
        for (kind, counts) in &self.classes {
            writeln!(
                w,
                "semantic: {kind}: base={} head={} identical={} differing={} message_differences={} evidence_differences={}",
                counts.base,
                counts.head,
                counts.identical,
                counts.differing,
                counts.messages,
                counts.evidence_differing
            )?;
        }
        for (name, kinds) in [
            (
                "target",
                &[
                    "trial.intent",
                    "trial.start",
                    "smu.intent",
                    "day.applied",
                    "climb.decision",
                ][..],
            ),
            ("schedule", &["night.plan", "task.start", "trial.start"][..]),
            (
                "permission",
                &[
                    "ccd.edge",
                    "unit.edge",
                    "unit.stop",
                    "climb.decision",
                    "night.qualification",
                    "day.freeze",
                    "day.veto",
                ][..],
            ),
            (
                "outcome",
                &[
                    "trial.end",
                    "trial.progress",
                    "failure",
                    "exposure.observed",
                ][..],
            ),
            (
                "replay",
                &[
                    "exposure.observed",
                    "session.imported",
                    "successor.imported",
                    "day.imported",
                    "trial.carried",
                    "failure.carried",
                    "state.rebuilt",
                ][..],
            ),
        ] {
            let (mut present, mut differing) = (0, 0);
            for kind in kinds {
                if let Some(counts) = self.classes.get(*kind) {
                    present += counts.base.max(counts.head);
                    differing += counts.evidence_differing;
                }
            }
            let verdict = if present == 0 {
                "absent"
            } else if differing == 0 {
                "identical"
            } else {
                "different"
            };
            writeln!(
                w,
                "semantic: {name} evidence {verdict}: records={present} differing={differing} (excluding msg only)"
            )?;
        }
        Ok(())
    }
}

pub struct Comparison {
    pub summary: Summary,
    pub first: Option<Difference>,
}

/// Compares every remaining JSON record, in order, after the declared field rebasing.
/// Missing journals and changed journal order are differences, not filtered data.
pub fn compare_dirs(
    base: &Path,
    head: &Path,
    retired: &[String],
) -> Result<Comparison, JournalError> {
    compare_with(&Disk, base, head, retired)
}

fn invalid(path: &Path, line: usize, reason: impl Into<String>) -> JournalError {
    JournalError::Semantic {
        path: path.to_path_buf(),
        line,
        reason: reason.into(),
    }
}

fn load(source: &impl Source, dir: &Path) -> Result<Journals, JournalError> {
    let files = source.files(dir)?;
    if files.is_empty() {
        return Err(JournalError::Missing);
    }
    let mut journals = BTreeMap::new();
    let mut total = 0;
    for (name, path) in files {
        let reader = source.open(&path).map_err(|source| JournalError::Read {
            path: path.clone(),
            source,
        })?;
        let mut records = Vec::new();
        for (number, line) in BufReader::new(reader).split(b'\n').enumerate() {
            let line = line.map_err(|source| JournalError::Read {
                path: path.clone(),
                source,
            })?;
            let normalized = normalize(&line).map_err(|source| JournalError::Decode {
                path: path.clone(),
                line: number + 1,
                source,
            })?;
            let event: Value =
                serde_json::from_slice(&normalized).map_err(|source| JournalError::Decode {
                    path: path.clone(),
                    line: number + 1,
                    source,
                })?;
            if !event.is_object()
                || event.get("kind").and_then(Value::as_str).is_none()
                || event.get("seq").and_then(Value::as_u64).is_none()
            {
                return Err(invalid(
                    &path,
                    number + 1,
                    "expected event object with kind and sequence",
                ));
            }
            records.push(event);
        }
        total += records.len();
        let session = records
            .iter()
            .find(|event| event["kind"] == "session.start")
            .and_then(|event| event.pointer("/data/session"))
            .and_then(Value::as_str)
            .map_or_else(|| name.clone(), str::to_owned);
        journals.insert(
            name,
            Journal {
                path,
                session,
                records,
                derived: Vec::new(),
            },
        );
    }
    if total == 0 {
        return Err(JournalError::Empty);
    }
    Ok(journals)
}

fn retain_and_index(
    journals: &mut Journals,
    retired: &[String],
    summary: &mut Summary,
) -> Result<Identities, JournalError> {
    let mut identities = Identities::new();
    let mut retired_counts: BTreeMap<&str, usize> =
        retired.iter().map(|kind| (kind.as_str(), 0)).collect();
    let mut rebased_cursor_causes = 0;
    for journal in journals.values_mut() {
        let mut map = BTreeMap::new();
        let mut ordinal = 0_u64;
        let (mut previous_retired, mut retained) = (None, None);
        for (line, event) in journal.records.iter_mut().enumerate() {
            let seq = event["seq"]
                .as_u64()
                .ok_or_else(|| invalid(&journal.path, line + 1, "invalid sequence"))?;
            if seq == 0 {
                return Err(invalid(&journal.path, line + 1, "zero sequence"));
            }
            let kind = event["kind"]
                .as_str()
                .ok_or_else(|| invalid(&journal.path, line + 1, "invalid event kind"))?;
            let canonical = if let Some(count) = retired_counts.get_mut(kind) {
                *count += 1;
                previous_retired = Some(seq);
                None
            } else {
                // Cause is the last-event cursor, not evidence. This is the
                // complete producer set: night_next::{plan_owed,schedule,dead_end}
                // and qualify::start_qualification. Other dependencies fail.
                let cursor = match kind {
                    "night.plan" => true,
                    "night.qualification" => event["data"]["state"]["kind"] == "start",
                    "deadend" => {
                        event["data"]["condition"] == "failure_at_zero"
                            && event["data"]["core"].is_null()
                            && event["data"]["action"].is_null()
                    }
                    _ => false,
                };
                if cursor
                    && let (Some(removed), Some(last)) = (previous_retired, retained)
                    && let Some(causes) = event["cause"].as_array_mut()
                {
                    let before = rebased_cursor_causes;
                    for cause in causes.iter_mut() {
                        if cause.as_u64() == Some(removed) {
                            *cause = Value::from(last);
                            rebased_cursor_causes += 1;
                        }
                    }
                    if rebased_cursor_causes != before {
                        causes.sort_by_key(Value::as_u64);
                        causes.dedup();
                    }
                }
                previous_retired = None;
                retained = Some(seq);
                ordinal += 1;
                Some(ordinal)
            };
            if map.insert(seq, canonical).is_some() {
                return Err(invalid(&journal.path, line + 1, "duplicate sequence"));
            }
        }
        if !retired.is_empty() {
            journal
                .records
                .retain(|event| event["seq"].as_u64().is_none_or(|seq| map[&seq].is_some()));
        }
        if identities.insert(journal.session.clone(), map).is_some() {
            return Err(invalid(&journal.path, 0, "duplicate session journal"));
        }
    }
    summary.retired.extend(
        retired_counts
            .into_iter()
            .map(|(kind, count)| (kind.to_owned(), count)),
    );
    summary.rebased_cursor_causes += rebased_cursor_causes;
    Ok(identities)
}

fn rebase_journal(journal: &mut Journal, identities: &Identities) -> Result<(), JournalError> {
    let map = &identities[&journal.session];
    journal.derived = derived::prepare(journal, map)?;
    for (line, event) in journal.records.iter_mut().enumerate() {
        rebase(
            event,
            "",
            &journal.session,
            identities,
            &journal.path,
            line + 1,
        )?;
    }
    Ok(())
}

// These are the journal's Seq fields (including transparent NightId), not durable
// ordinals such as DecisionId/ApplicationId, trial ids, offsets or durations.
fn reference(field: &str) -> bool {
    matches!(
        field,
        "seq"
            | "cause"
            | "causes"
            | "night"
            | "last_checkpoint"
            | "witness"
            | "witnesses"
            | "exposure"
            | "failure"
            | "failures"
            | "retreat"
            | "evidence"
            | "caps"
            | "qualification"
            | "vetoes"
            | "events"
            | "hours"
            | "transactions"
            | "credited"
            | "checkpoints"
            | "edges"
            | "certificate"
            | "ceilings"
            | "decisions"
            | "known_failure"
            | "in_flight"
            | "source_seqs"
            | "parked_seq"
            | "base_seq"
            | "passes"
    )
}

fn rebase(
    value: &mut Value,
    field: &str,
    session: &str,
    identities: &Identities,
    path: &Path,
    line: usize,
) -> Result<(), JournalError> {
    match value {
        Value::Object(object) => {
            let context = object
                .get("session")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let context = context.as_deref().unwrap_or(session);
            let imported_session = object
                .get("original_kind")
                .and_then(Value::as_str)
                .and_then(|_| object.get("original"))
                .and_then(|original| original.get("session"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            for (field, child) in object {
                if field == "build" {
                    continue;
                }
                if field == "raw"
                    && let Some(imported_session) = &imported_session
                    && let Some(raw) = child.as_str()
                {
                    let mut payload: Value =
                        serde_json::from_str(raw).map_err(|source| JournalError::Decode {
                            path: path.to_path_buf(),
                            line,
                            source,
                        })?;
                    rebase(&mut payload, "", imported_session, identities, path, line)?;
                    *child = Value::String(payload.to_string());
                    continue;
                }
                rebase(child, field, context, identities, path, line)?;
            }
        }
        Value::Array(array) => {
            for child in array {
                rebase(child, field, session, identities, path, line)?;
            }
        }
        Value::Number(number) if reference(field) => {
            // Original identities from external facts/sessions are immutable inputs.
            if let Some(map) = identities.get(session) {
                let seq = number.as_u64().ok_or_else(|| {
                    invalid(path, line, format!("invalid {field} reference {number}"))
                })?;
                match map.get(&seq) {
                    Some(Some(canonical)) => *value = Value::from(*canonical),
                    Some(None) => {
                        return Err(invalid(
                            path,
                            line,
                            format!(
                                "{field} references retired record {session}#{seq}; semantic dependency cannot be discarded"
                            ),
                        ));
                    }
                    None => {
                        return Err(invalid(
                            path,
                            line,
                            format!("unresolved {field} reference {session}#{seq}"),
                        ));
                    }
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Number(_) => {}
    }
    Ok(())
}

fn evidence_equal(base: &Value, head: &Value) -> bool {
    match (base.as_object(), head.as_object()) {
        (Some(base), Some(head)) => base
            .iter()
            .filter(|(key, _)| key.as_str() != "msg")
            .eq(head.iter().filter(|(key, _)| key.as_str() != "msg")),
        _ => false,
    }
}

fn counts(base: &[Value], head: &[Value], summary: &mut Summary) {
    let mut classes: BTreeMap<&str, [Vec<&Value>; 2]> = BTreeMap::new();
    for (side, records) in [base, head].into_iter().enumerate() {
        for event in records {
            if let Some(kind) = event["kind"].as_str() {
                classes.entry(kind).or_default()[side].push(event);
            }
        }
    }
    for (kind, [base, head]) in classes {
        let total = summary.classes.entry(kind.to_owned()).or_default();
        total.base += base.len();
        total.head += head.len();
        for i in 0..base.len().max(head.len()) {
            if let (Some(base), Some(head)) = (base.get(i), head.get(i)) {
                if base == head {
                    total.identical += 1;
                } else {
                    total.differing += 1;
                }
                total.messages += usize::from(base.get("msg") != head.get("msg"));
                total.evidence_differing += usize::from(!evidence_equal(base, head));
            } else {
                total.differing += 1;
                total.evidence_differing += 1;
            }
        }
    }
}

pub(in crate::same) fn compare_with(
    source: &impl Source,
    base: &Path,
    head: &Path,
    retired: &[String],
) -> Result<Comparison, JournalError> {
    let mut base = load(source, base)?;
    let mut head = load(source, head)?;
    let mut summary = Summary::default();
    let base_identities = retain_and_index(&mut base, retired, &mut summary)?;
    let head_identities = retain_and_index(&mut head, &[], &mut summary)?;
    for (name, base) in &mut base {
        if let Some(head) = head.get_mut(name) {
            let verified = derived::verified_messages(base, head);
            rebase_journal(base, &base_identities)?;
            rebase_journal(head, &head_identities)?;
            derived::rebase_messages(base, head, &verified, &mut summary);
            derived::compare_fields(base, head, &mut summary);
        } else {
            rebase_journal(base, &base_identities)?;
        }
    }
    for (name, head) in &mut head {
        if !base.contains_key(name) {
            rebase_journal(head, &head_identities)?;
        }
    }
    let names: BTreeSet<&String> = base.keys().chain(head.keys()).collect();
    let mut first = None;
    for name in names {
        let journals = [base.get(name), head.get(name)];
        let records =
            journals.map(|journal| journal.map_or(&[][..], |journal| journal.records.as_slice()));
        counts(records[0], records[1], &mut summary);
        if first.is_none() {
            let mismatch = (0..records[0].len().max(records[1].len()))
                .find(|&i| records[0].get(i) != records[1].get(i));
            if mismatch.is_some() || journals.iter().any(Option::is_none) {
                let i = mismatch.unwrap_or(0);
                let show = |side: usize| {
                    if journals[side].is_none() {
                        "<missing file>".to_owned()
                    } else {
                        records[side]
                            .get(i)
                            .map_or_else(|| "<missing event>".to_owned(), Value::to_string)
                    }
                };
                first = Some(Difference {
                    file: name.clone(),
                    line: Some(i + 1),
                    base: show(0),
                    head: show(1),
                });
            }
        }
    }
    Ok(Comparison { summary, first })
}

#[cfg(test)]
mod tests;
