use std::num::NonZeroU32;

use togi_journal::{Decision, DefectDirection, EventKind};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DefectId(pub NonZeroU32);

impl DefectId {
    pub const fn new(id: u32) -> Option<Self> {
        match NonZeroU32::new(id) {
            Some(id) => Some(Self(id)),
            None => None,
        }
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FixStamp(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Clause {
    pub decision_kind: EventKind,
    pub decision: Decision,
    pub cause_kind: EventKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    pub id: DefectId,
    pub title: &'static str,
    pub detail: &'static str,
    pub pr: u32,
    pub direction: DefectDirection,
    pub clauses: &'static [Clause],
}

pub static KNOWN_DEFECTS: &[Entry] = &[];

const _: () = assert!(is_ordered(KNOWN_DEFECTS));

pub const fn is_ordered(registry: &[Entry]) -> bool {
    let mut at = 1;
    while at < registry.len() {
        if registry[at - 1].id.get() >= registry[at].id.get() {
            return false;
        }
        at += 1;
    }
    true
}

pub const fn fixed_stamp(registry: &[Entry]) -> FixStamp {
    match registry.last() {
        Some(entry) => FixStamp(entry.id.get()),
        None => FixStamp(0),
    }
}
