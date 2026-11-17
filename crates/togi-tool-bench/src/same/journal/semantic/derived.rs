//! Sequence-derived references and random-phase telemetry, never tuner decisions.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;
use togi_journal::{Event, EventKind, Payload, TrialSignal};

use super::{Journal, JournalError, Summary, invalid};

#[derive(Default)]
pub(super) struct Record {
    role_seed: Option<u64>,
    signal_seed: Option<u64>,
    phase_seed: Option<u64>,
}

fn first_cause(event: &Value) -> Option<u64> {
    event.pointer("/cause/0")?.as_u64()
}

fn rebase_seed(
    value: &mut Value,
    intent: Option<u64>,
    identities: &super::Sequences,
    path: &std::path::Path,
    line: usize,
) -> Result<u64, JournalError> {
    // Like an envelope cursor, this is an identity: run_trial constructs the
    // role and TrialSpec from its own trial.intent sequence, not a free seed.
    let Some(intent) = intent.filter(|&seq| value.as_u64() == Some(seq)) else {
        return Err(invalid(
            path,
            line,
            "random load seed is not this trial's own intent sequence",
        ));
    };
    let Some(Some(canonical)) = identities.get(&intent) else {
        return Err(invalid(
            path,
            line,
            "random load seed has no retained intent",
        ));
    };
    *value = Value::from(*canonical);
    Ok(intent)
}

pub(super) fn prepare(
    journal: &mut Journal,
    identities: &super::Sequences,
) -> Result<Vec<Record>, JournalError> {
    let mut intents = HashMap::new();
    let mut starts = HashMap::new();
    let mut phase_seeds = HashMap::new();
    let mut derived = Vec::with_capacity(journal.records.len());
    for (index, event) in journal.records.iter_mut().enumerate() {
        let mut record = Record::default();
        let line = index + 1;
        let seq = event["seq"]
            .as_u64()
            .expect("canonicalize validated the sequence");
        match event["kind"].as_str() {
            Some("trial.intent") => {
                if let Some(trial) = event["data"]["trial"].as_str() {
                    intents.insert(seq, trial.to_owned());
                }
            }
            Some("trial.start") => {
                let trial = event["data"]["trial"].as_str().map(str::to_owned);
                let intent = first_cause(event)
                    .filter(|seq| intents.get(seq) == trial.as_ref() && trial.is_some());
                if let Some(roles) = event["data"].get_mut("roles").and_then(Value::as_array_mut) {
                    for role in roles {
                        if role["schedule"]["load"]["kind"] == "random" {
                            record.role_seed = Some(rebase_seed(
                                &mut role["schedule"]["load"]["seed"],
                                intent,
                                identities,
                                &journal.path,
                                line,
                            )?);
                        }
                    }
                }
                if let (Some(trial), Some(intent)) = (trial, intent) {
                    starts.insert(seq, (trial, intent));
                }
            }
            Some("trial.signal") => {
                let start = first_cause(event).filter(|seq| {
                    starts.get(seq).is_some_and(|(trial, _)| {
                        event["data"]["trial"].as_str() == Some(trial.as_str())
                    })
                });
                if event["data"]["schedule"]["kind"] == "random" {
                    let intent = start.map(|seq| starts[&seq].1);
                    let seed = rebase_seed(
                        &mut event["data"]["schedule"]["seed"],
                        intent,
                        identities,
                        &journal.path,
                        line,
                    )?;
                    record.signal_seed = Some(seed);
                    phase_seeds.insert(start.expect("validated signal start"), seed);
                } else if event["data"]["schedule"].is_null() {
                    record.phase_seed = start.and_then(|seq| phase_seeds.get(&seq).copied());
                }
            }
            _ => {}
        }
        derived.push(record);
    }
    Ok(derived)
}

/// Only consider mismatching messages proven identical to each side's own renderer.
pub(super) fn verified_messages(base: &Journal, head: &Journal) -> Vec<(usize, EventKind)> {
    base.records
        .iter()
        .zip(&head.records)
        .enumerate()
        .filter_map(|(index, (base, head))| {
            if base["msg"] == head["msg"] || base["kind"] != head["kind"] {
                return None;
            }
            let base = Event::decode(&base.to_string()).ok()?;
            let head = Event::decode(&head.to_string()).ok()?;
            (base.msg == base.payload.message() && head.msg == head.payload.message())
                .then(|| (index, base.kind()))
        })
        .collect()
}

pub(super) fn rebase_messages(
    base: &mut Journal,
    head: &mut Journal,
    verified: &[(usize, EventKind)],
    summary: &mut Summary,
) {
    for &(index, kind) in verified {
        let base = &mut base.records[index];
        let head = &mut head.records[index];
        // Full payload equality must already hold under sequence rebasing.
        // Render it once: both original strings were independently verified,
        // so only the canonical payload can account for the new wording.
        if base["data"] == head["data"]
            && let Ok(canonical) = Event::decode(&base.to_string())
        {
            let message = Value::from(canonical.payload.message());
            base["msg"] = message.clone();
            head["msg"] = message;
            *summary.rebased_messages.entry(kind).or_default() += 1;
        }
    }
}

fn generated_counts(event: &Value) -> Option<TrialSignal> {
    let signal = TrialSignal::deserialize(&event["data"]).ok()?;
    (signal.schedule.is_none()
        && event["msg"].as_str() == Some(Payload::TrialSignal(signal.clone()).message().as_str()))
    .then_some(signal)
}

pub(super) fn compare_fields(base: &mut Journal, head: &mut Journal, summary: &mut Summary) {
    for ((base, b), (head, h)) in base
        .records
        .iter_mut()
        .zip(&base.derived)
        .zip(head.records.iter_mut().zip(&head.derived))
    {
        for (field, changed) in [
            (
                "trial.start.data.roles[*].schedule.load.seed",
                b.role_seed.zip(h.role_seed).is_some_and(|(b, h)| b != h),
            ),
            (
                "trial.signal.data.schedule.seed",
                b.signal_seed
                    .zip(h.signal_seed)
                    .is_some_and(|(b, h)| b != h),
            ),
        ] {
            summary.declare(field, changed);
        }
        // Only paired telemetry from a verified random phase with a moved
        // seed can vary. Signal kind, trial/cause, timestamps and outcome
        // records still go through the full-record comparison below.
        if b.phase_seed.zip(h.phase_seed).is_some_and(|(b, h)| b != h)
            && let (Some(b), Some(h)) = (generated_counts(base), generated_counts(head))
            && b.trial == h.trial
        {
            for (field, label, changed) in [
                ("stops", "trial.signal.data.stops", b.stops != h.stops),
                ("conts", "trial.signal.data.conts", b.conts != h.conts),
            ] {
                summary.declare(label, changed);
                if changed {
                    for event in [&mut *base, &mut *head] {
                        event["data"]
                            .as_object_mut()
                            .expect("validated signal")
                            .remove(field);
                    }
                }
            }
            let message_changed = base["msg"] != head["msg"];
            summary.declare("trial.signal.msg.phase_counts", message_changed);
            if message_changed {
                let mut counts = b;
                if counts.stops != h.stops {
                    counts.stops = 0;
                }
                if counts.conts != h.conts {
                    counts.conts = 0;
                }
                let message = Value::from(Payload::TrialSignal(counts).message());
                base["msg"] = message.clone();
                head["msg"] = message;
            }
        }
    }
}
