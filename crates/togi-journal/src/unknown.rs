//! Finding an unknown field of a journal line and cutting it out of the line's text.
//!
//! The journal types keep `deny_unknown_fields`, so every decode that meets an unknown field fails with serde's
//! "unknown field" error. Only then does this module run: [`locate`] turns that error into the field's place in the
//! line, and [`Unknown::remove_from`] returns the line without it, so a lenient read decodes the rest and says what it
//! ignored. Types embedded from other crates (`togi-machine`, `togi-config`) stay strict wherever else they are read.

use std::fmt::{self, Write as _};

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;
use serde_path_to_error::Segment;

/// One step from the root of a line to a value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    Key(String),
    Index(usize),
}

/// A field a decode refused because the type has no such field: the object that holds it and its name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Unknown {
    container: Vec<Step>,
    field: String,
}

/// How serde starts its refusal of an unknown field. This module reads serde's wording as a protocol;
/// `serde_refuses_an_unknown_field_in_the_words_locate_reads` pins it.
const REFUSAL: &str = "unknown field `";

/// Whether `error` is serde's refusal of a field the type does not have.
pub(crate) fn is_unknown_field(error: &serde_json::Error) -> bool {
    error.is_data() && error.to_string().starts_with(REFUSAL)
}

/// The field that `error` refused, found in `line`. `prefix` is where the decoded value sits in `line`, and `decode`
/// decodes that value again, as the decode that failed did.
///
/// The path of a refused field ends at its key, except inside an internally tagged enum: serde buffers one before
/// decoding it, so the path of a field refused anywhere in it, at any depth, ends at the enum's object. The refused
/// field is therefore one of the keys the error names: the last step of the path, if its parent holds that key, and
/// every key of that name under the object the path ends at. When there is more than one, some are fields the type
/// knows, so each is tried by renaming it to a name no type has and decoding again. A decode meets keys in the order
/// the line has them, and the candidates are in that order: renaming one met before the refused field, or the
/// refused field itself, makes the decode refuse the new name, and renaming one met after it does not. The refused
/// field is the last candidate whose renaming is refused.
pub(crate) fn locate(
    line: &str,
    prefix: &[Step],
    error: &serde_path_to_error::Error<serde_json::Error>,
    decode: impl Fn(&str) -> serde_json::Result<()>,
) -> Option<Unknown> {
    let message = error.inner().to_string();
    let refused = message.strip_prefix(REFUSAL)?;
    let mut steps = prefix.to_vec();
    for segment in error.path() {
        steps.push(match segment {
            Segment::Seq { index } => Step::Index(*index),
            Segment::Map { key } => Step::Key(key.clone()),
            Segment::Enum { variant } => Step::Key(variant.clone()),
            Segment::Unknown => return None,
        });
    }
    let mut candidates = Vec::new();
    if steps.len() > prefix.len()
        && let Some(Step::Key(key)) = steps.last()
        && names(refused, key)
    {
        let container = &steps[..steps.len() - 1];
        if keys_at(line, container)?.contains(key) {
            candidates.push(Unknown {
                container: container.to_vec(),
                field: key.clone(),
            });
        }
    }
    collect(
        at(line, &steps)?.get(),
        &mut steps,
        refused,
        &mut candidates,
    );
    if candidates.len() <= 1 {
        return candidates.pop();
    }
    candidates.into_iter().rev().find(|candidate| {
        let renamed = rewrite(
            line,
            &candidate.container,
            &candidate.field,
            Edit::Rename(PROBE),
        );
        renamed
            .and_then(|renamed| at(&renamed, prefix))
            .and_then(|value| decode(value.get()).err())
            .and_then(|error| {
                let message = error.to_string();
                let refused = message.strip_prefix(REFUSAL)?;
                names(refused, PROBE).then_some(())
            })
            .is_some()
    })
}

/// The key a field is renamed to when it is tried; no journal type has a field of this name.
const PROBE: &str = "\u{0}togi unknown field probe";

/// Every key `refused` names in the objects under `text`, which sits at `path` in the line, in the order a decode
/// meets them. Of a key an object repeats, only the first is listed or entered: a decode refuses the repeat before it
/// gets past it.
fn collect(text: &str, path: &mut Vec<Step>, refused: &str, found: &mut Vec<Unknown>) {
    if let Some(pairs) = members(text) {
        let mut seen: Vec<&str> = Vec::new();
        for (key, value) in &pairs {
            if seen.contains(&key.as_str()) {
                continue;
            }
            seen.push(key);
            if names(refused, key) {
                found.push(Unknown {
                    container: path.clone(),
                    field: key.clone(),
                });
            }
            path.push(Step::Key(key.clone()));
            collect(value.get(), path, refused, found);
            path.pop();
        }
    } else if let Some(items) = elements(text) {
        for (index, item) in items.iter().enumerate() {
            path.push(Step::Index(index));
            collect(item.get(), path, refused, found);
            path.pop();
        }
    }
}

/// Whether `refused`, the text after serde's refusal prefix, starts with `key` as the whole field name.
fn names(refused: &str, key: &str) -> bool {
    refused
        .strip_prefix(key)
        .is_some_and(|rest| rest.starts_with("`, "))
}

impl Unknown {
    /// The field's place in the line: its keys and indices joined, for example `data.members[1].extra`.
    pub(crate) fn path(&self) -> String {
        let mut path = String::new();
        for step in &self.container {
            match step {
                Step::Key(key) => push_key(&mut path, key),
                Step::Index(index) => {
                    let _ = write!(path, "[{index}]");
                }
            }
        }
        push_key(&mut path, &self.field);
        path
    }

    /// `line` without the field; every other byte of every other value is kept, duplicate keys included. `None` when
    /// the line holds no such field at that place.
    pub(crate) fn remove_from(&self, line: &str) -> Option<String> {
        rewrite(line, &self.container, &self.field, Edit::Remove)
    }
}

fn push_key(path: &mut String, key: &str) {
    if !path.is_empty() {
        path.push('.');
    }
    path.push_str(key);
}

type Pairs = Vec<(String, Box<RawValue>)>;

/// An object's members in source order, duplicates kept.
struct Members(Pairs);

impl<'de> Deserialize<'de> for Members {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Collect;

        impl<'de> Visitor<'de> for Collect {
            type Value = Members;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Members, A::Error> {
                let mut pairs = Vec::new();
                while let Some(pair) = map.next_entry::<String, Box<RawValue>>()? {
                    pairs.push(pair);
                }
                Ok(Members(pairs))
            }
        }

        deserializer.deserialize_map(Collect)
    }
}

fn members(text: &str) -> Option<Pairs> {
    serde_json::from_str::<Members>(text)
        .ok()
        .map(|Members(pairs)| pairs)
}

fn elements(text: &str) -> Option<Vec<Box<RawValue>>> {
    serde_json::from_str(text).ok()
}

fn at(line: &str, steps: &[Step]) -> Option<Box<RawValue>> {
    let mut current: Box<RawValue> = serde_json::from_str(line).ok()?;
    for step in steps {
        current = match step {
            Step::Key(key) => {
                members(current.get())?
                    .into_iter()
                    .find(|(name, _)| name == key)?
                    .1
            }
            Step::Index(index) => elements(current.get())?.into_iter().nth(*index)?,
        };
    }
    Some(current)
}

fn keys_at(line: &str, steps: &[Step]) -> Option<Vec<String>> {
    let pairs = members(at(line, steps)?.get())?;
    Some(pairs.into_iter().map(|(key, _)| key).collect())
}

fn object(pairs: &[(String, Box<RawValue>)]) -> Option<String> {
    let mut text = String::from("{");
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&serde_json::to_string(key).ok()?);
        text.push(':');
        text.push_str(value.get());
    }
    text.push('}');
    Some(text)
}

fn array(elements: &[Box<RawValue>]) -> String {
    let mut text = String::from("[");
    for (index, element) in elements.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push_str(element.get());
    }
    text.push(']');
    text
}
/// What [`rewrite`] does to a field: cut every occurrence of it, or rename its first occurrence.
#[derive(Clone, Copy)]
enum Edit<'a> {
    Remove,
    Rename(&'a str),
}

fn rewrite(text: &str, steps: &[Step], field: &str, edit: Edit<'_>) -> Option<String> {
    match steps.split_first() {
        None => {
            let mut pairs = members(text)?;
            match edit {
                Edit::Remove => {
                    let before = pairs.len();
                    pairs.retain(|(key, _)| key != field);
                    if pairs.len() == before {
                        return None;
                    }
                }
                Edit::Rename(name) => {
                    let slot = pairs.iter_mut().find(|(key, _)| key == field)?;
                    name.clone_into(&mut slot.0);
                }
            }
            object(&pairs)
        }
        Some((Step::Key(key), rest)) => {
            let mut pairs = members(text)?;
            let slot = pairs.iter_mut().find(|(name, _)| name == key)?;
            slot.1 = RawValue::from_string(rewrite(slot.1.get(), rest, field, edit)?).ok()?;
            object(&pairs)
        }
        Some((Step::Index(index), rest)) => {
            let mut items = elements(text)?;
            let slot = items.get_mut(*index)?;
            *slot = RawValue::from_string(rewrite(slot.get(), rest, field, edit)?).ok()?;
            Some(array(&items))
        }
    }
}
