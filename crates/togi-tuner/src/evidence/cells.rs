use std::collections::HashMap;

use crate::{Profile, Seq};

use super::BackendId;

/// Seqs in ascending order: they are only ever appended, and each lookup is a binary search.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Seqs(Vec<Seq>);

impl Seqs {
    pub(super) fn push(&mut self, seq: Seq) {
        debug_assert!(self.0.last().is_none_or(|&last| last < seq));
        self.0.push(seq);
    }

    pub(super) fn last(&self) -> Option<Seq> {
        self.0.last().copied()
    }

    /// The seqs above `floor`.
    pub(super) fn after(&self, floor: Option<Seq>) -> &[Seq] {
        let start = self.0.partition_point(|&seq| Some(seq) <= floor);
        &self.0[start..]
    }

    pub(super) fn remove(&mut self, seq: Seq) {
        if let Ok(at) = self.0.binary_search(&seq) {
            self.0.remove(at);
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = Seq> + '_ {
        self.0.iter().copied()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FailureCell {
    pub(super) profile: Profile,
    pub(super) live: Seqs,
    pub(super) carried: Seqs,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PassCell {
    pub(super) backend: BackendId,
    pub(super) profile: Profile,
    pub(super) live: Seqs,
    pub(super) live_non_alone: Seqs,
    pub(super) carried: Seqs,
}

/// Failure cells, one per distinct profile.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct FailureCells {
    cells: Vec<FailureCell>,
    index: HashMap<Profile, usize>,
}

impl FailureCells {
    pub(super) fn cell_mut(&mut self, profile: Profile) -> &mut FailureCell {
        let at = *self.index.entry(profile).or_insert_with(|| {
            self.cells.push(FailureCell {
                profile,
                live: Seqs::default(),
                carried: Seqs::default(),
            });
            self.cells.len() - 1
        });
        &mut self.cells[at]
    }

    pub(super) fn cell_at_mut(&mut self, profile: &Profile) -> Option<&mut FailureCell> {
        let at = *self.index.get(profile)?;
        self.cells.get_mut(at)
    }

    pub(super) fn cells(&self) -> &[FailureCell] {
        &self.cells
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &FailureCell> {
        self.cells.iter()
    }
}

/// Pass cells, one per backend package and distinct profile.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct PassCells {
    cells: Vec<PassCell>,
    index: HashMap<(BackendId, Profile), usize>,
}

impl PassCells {
    pub(super) fn cell_mut(&mut self, backend: BackendId, profile: Profile) -> &mut PassCell {
        let at = *self.index.entry((backend, profile)).or_insert_with(|| {
            self.cells.push(PassCell {
                backend,
                profile,
                live: Seqs::default(),
                live_non_alone: Seqs::default(),
                carried: Seqs::default(),
            });
            self.cells.len() - 1
        });
        &mut self.cells[at]
    }

    pub(super) fn cell_at_mut(
        &mut self,
        backend: BackendId,
        profile: &Profile,
    ) -> Option<&mut PassCell> {
        let at = *self.index.get(&(backend, *profile))?;
        self.cells.get_mut(at)
    }

    pub(super) fn cells(&self) -> &[PassCell] {
        &self.cells
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ClassCells {
    pub(super) failures: FailureCells,
    pub(super) passes: PassCells,
    pub(super) exposure: u32,
}
