use togi_journal::{DefectDirection, DefectId, EventKind, FixStamp};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Clause {
    /// The kind of the Ruleset 13 decision event: `ccd.edge`, `ccd.cap`, `night.plan`, `night.qualification` or
    /// `night.reproduction`.
    pub decision_kind: EventKind,
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
        Some(entry) => FixStamp::through(entry.id),
        None => FixStamp::none(),
    }
}
