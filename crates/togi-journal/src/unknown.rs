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

const REFUSAL: &str = "unknown field `";

/// Whether `error` is serde's refusal of a field the type does not have.
pub(crate) fn is_unknown_field(error: &serde_json::Error) -> bool {
    error.is_data() && error.to_string().starts_with(REFUSAL)
}

/// The field that `error` refused, found in `line`. `prefix` is where the decoded value sits in `line`.
///
/// serde buffers an internally tagged enum before decoding it, so the path of a field refused inside one ends at the
/// enum's object and does not name the field; the field is then looked up among that object's keys. A field refused
/// deeper inside such an enum's variant is not found.
pub(crate) fn locate(
    line: &str,
    prefix: &[Step],
    error: &serde_path_to_error::Error<serde_json::Error>,
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
    if let Some(Step::Key(key)) = steps.last()
        && names(refused, key)
    {
        let field = key.clone();
        steps.pop();
        if keys_at(line, &steps)?.contains(&field) {
            return Some(Unknown {
                container: steps,
                field,
            });
        }
        steps.push(Step::Key(field));
    }
    let field = keys_at(line, &steps)?
        .into_iter()
        .find(|key| names(refused, key))?;
    Some(Unknown {
        container: steps,
        field,
    })
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
        rewrite(line, &self.container, &self.field)
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

fn rewrite(text: &str, steps: &[Step], field: &str) -> Option<String> {
    match steps.split_first() {
        None => {
            let mut pairs = members(text)?;
            let before = pairs.len();
            pairs.retain(|(key, _)| key != field);
            (pairs.len() != before).then(|| object(&pairs))?
        }
        Some((Step::Key(key), rest)) => {
            let mut pairs = members(text)?;
            let slot = pairs.iter_mut().find(|(name, _)| name == key)?;
            slot.1 = RawValue::from_string(rewrite(slot.1.get(), rest, field)?).ok()?;
            object(&pairs)
        }
        Some((Step::Index(index), rest)) => {
            let mut items = elements(text)?;
            let slot = items.get_mut(*index)?;
            *slot = RawValue::from_string(rewrite(slot.get(), rest, field)?).ok()?;
            Some(array(&items))
        }
    }
}
