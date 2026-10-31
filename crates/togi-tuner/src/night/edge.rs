//! The night edge U and the per-unit mapping cursor (`docs/spec/tuner.md`, Night edge U and Night
//! order and reserved budgets).

use std::collections::BTreeMap;
use std::ops::Bound;

use crate::UnitId;
use togi_journal::Seq;
use togi_machine::{MIN_OFFSET, TargetOffset};

/// Counts between coarse mapping steps.
pub const COARSE_STEP: i64 = 5;
/// Counts between bracket steps.
pub const BRACKET_STEP: i64 = 1;

/// The deepest offset the SMU accepts: where a clean mapping descent ends censored.
#[must_use]
pub fn write_limit() -> TargetOffset {
    TargetOffset::clamped(MIN_OFFSET)
}

fn offset_of(counts: i64) -> TargetOffset {
    TargetOffset::clamped(counts)
}

/// A unit's night edge U. Absence is never offset zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "edge", rename_all = "snake_case")]
pub enum EdgeU {
    /// No failure yet, and the write limit has not been passed cleanly.
    Unknown,
    /// The shallowest actual failure on the tested unit, kept even when a later launch passes there.
    Observed { offset: TargetOffset, witness: Seq },
    /// The full mapping matrix passed at the write limit: no failure, so no invented one.
    Censored,
}

impl EdgeU {
    /// The numeric boundary: the failure, or the write limit for a censored edge.
    #[must_use]
    pub fn offset(&self) -> Option<TargetOffset> {
        match self {
            Self::Unknown => None,
            Self::Observed { offset, .. } => Some(*offset),
            Self::Censored => Some(write_limit()),
        }
    }

    /// The hard night ceiling C; unknown evidence supplies no ceiling.
    #[must_use]
    pub fn ceiling(&self) -> Option<TargetOffset> {
        self.offset()
            .map(|offset| TargetOffset::clamped(offset.get() + i64::from(crate::cap::EDGE_MARGIN)))
    }
    /// This edge after a failure charged to the tested unit. A shallower failure replaces a deeper
    /// one; a censored or unknown edge takes any failure.
    #[must_use]
    pub fn with_failure(self, offset: TargetOffset, witness: Seq) -> Self {
        match self {
            Self::Observed { offset: kept, .. } if kept >= offset => self,
            Self::Observed { .. } | Self::Unknown | Self::Censored => {
                Self::Observed { offset, witness }
            }
        }
    }
}

/// Where a conclusive failure of an isolated trial was located.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Location {
    /// On the tested unit, including an unattributed controlled isolated failure and togi's own
    /// controller, dashboard and workers running there.
    TestedCcd,
    /// Visible, but not charged to the tested unit.
    Elsewhere,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum IsolatedResult {
    Pass,
    Fail { location: Location, witness: Seq },
}

/// A conclusive isolated mapping or reproduction observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IsolatedObservation {
    pub unit: UnitId,
    pub offset: TargetOffset,
    pub result: IsolatedResult,
}

/// What a finished mapping step reports to its cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum StepResult {
    /// Every cell of the step's mapping matrix passed (or was already covered).
    Passed,
    /// The step stopped at its first failure charged to the tested unit.
    Failed { witness: Seq },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingKind {
    Coarse,
    Bracket,
    Sweep,
}

/// Where a unit's mapping goes next, in counts (-5 to -50, `Sweep` up toward C, never 0).
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "cursor", rename_all = "snake_case")]
pub enum Cursor {
    /// Descending by five counts from the implicit zero last pass; `offset` is the step in
    /// progress, first -5.
    Coarse { offset: i64 },
    /// Descending by one count from the last passing coarse step toward `failed`, the first
    /// failing coarse step; `next` is the offset in progress.
    Bracket { failed: i64, next: i64 },
    /// Each integer above U toward C, for cells still uncovered; `next` is the offset in progress.
    Sweep { next: i64 },
}

/// One unit's edge and cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UnitMapping {
    edge: EdgeU,
    cursor: Cursor,
}

impl Default for UnitMapping {
    fn default() -> Self {
        Self {
            edge: EdgeU::Unknown,
            cursor: Cursor::Coarse {
                offset: -COARSE_STEP,
            },
        }
    }
}

impl UnitMapping {
    #[must_use]
    pub const fn edge(&self) -> EdgeU {
        self.edge
    }

    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// The next offset to map and why, or `None` when the descent and bracket are done and the
    /// sweep has reached `ceiling`, the unit's current C. An offset whose cells are all covered finishes
    /// as [`StepResult::Passed`] without spending night time.
    #[must_use]
    pub fn target(&self, ceiling: TargetOffset) -> Option<(TargetOffset, MappingKind)> {
        match self.cursor {
            Cursor::Coarse { offset } => Some((offset_of(offset), MappingKind::Coarse)),
            Cursor::Bracket { next, .. } => Some((offset_of(next), MappingKind::Bracket)),
            Cursor::Sweep { next } => {
                let offset = TargetOffset::new(next).ok()?;
                (offset < TargetOffset::ZERO && offset <= ceiling)
                    .then_some((offset, MappingKind::Sweep))
            }
        }
    }

    fn edge_counts(&self) -> i64 {
        self.edge.offset().map_or(0, TargetOffset::get)
    }

    /// Bracket from the last passing coarse step `passed` toward the failing step `failed`, or
    /// sweep when there is nothing left to bracket.
    fn after_failure(&mut self, failed: i64, passed: Option<i64>) {
        let edge = self.edge_counts();
        self.cursor = match passed {
            Some(passed) if passed - BRACKET_STEP > failed && edge < passed - BRACKET_STEP => {
                Cursor::Bracket {
                    failed,
                    next: passed - BRACKET_STEP,
                }
            }
            Some(_) | None => Cursor::Sweep { next: edge + 1 },
        };
    }

    /// Fold the result of the step the cursor is on.
    pub fn finish(&mut self, result: StepResult) {
        match (self.cursor, result) {
            (Cursor::Coarse { offset }, StepResult::Passed) => {
                let next = offset - COARSE_STEP;
                if next >= write_limit().get() {
                    self.cursor = Cursor::Coarse { offset: next };
                } else {
                    if self.edge == EdgeU::Unknown {
                        self.edge = EdgeU::Censored;
                    }
                    self.cursor = Cursor::Sweep {
                        next: self.edge_counts() + 1,
                    };
                }
            }
            (Cursor::Coarse { offset }, StepResult::Failed { witness }) => {
                self.edge = self.edge.with_failure(offset_of(offset), witness);
                self.after_failure(offset, Some(offset + COARSE_STEP));
            }
            (Cursor::Bracket { failed, next }, StepResult::Passed) => {
                let following = next - BRACKET_STEP;
                self.cursor = if following > failed {
                    Cursor::Bracket {
                        failed,
                        next: following,
                    }
                } else {
                    Cursor::Sweep {
                        next: self.edge_counts() + 1,
                    }
                };
            }
            (
                Cursor::Bracket { next, .. } | Cursor::Sweep { next },
                StepResult::Failed { witness },
            ) => {
                self.edge = self.edge.with_failure(offset_of(next), witness);
                self.cursor = Cursor::Sweep {
                    next: self.edge_counts() + 1,
                };
            }
            (Cursor::Sweep { next }, StepResult::Passed) => {
                self.cursor = Cursor::Sweep { next: next + 1 };
            }
        }
    }

    /// Fold an isolated observation from any source. Only a failure charged to the tested unit
    /// moves the edge; passes never undo it, and a failure elsewhere is not charged.
    pub fn observe(&mut self, offset: TargetOffset, result: IsolatedResult) {
        let IsolatedResult::Fail {
            location: Location::TestedCcd,
            witness,
        } = result
        else {
            return;
        };
        let before = self.edge;
        self.edge = self.edge.with_failure(offset, witness);
        if self.edge == before {
            return;
        }
        let edge = self.edge_counts();
        match self.cursor {
            Cursor::Coarse { offset: step } if edge >= step => {
                self.after_failure(edge, Some(step + COARSE_STEP));
            }
            Cursor::Coarse { .. } => {}
            Cursor::Bracket { .. } => {
                self.cursor = Cursor::Sweep { next: edge + 1 };
            }
            Cursor::Sweep { next } => {
                self.cursor = Cursor::Sweep {
                    next: next.max(edge + 1),
                };
            }
        }
    }
}

/// A mapping offset to run: which unit, at what offset, and why that one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MappingTarget {
    pub unit: UnitId,
    pub offset: TargetOffset,
    pub kind: MappingKind,
}

/// Every unit's mapping, visited round-robin in ascending unit order, one mapping offset per unit
/// before returning to the first.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MappingBoard {
    #[serde(with = "togi_machine::serde_pairs")]
    units: BTreeMap<UnitId, UnitMapping>,
    /// The unit whose offset was mapped last.
    visited: Option<UnitId>,
}

impl MappingBoard {
    #[must_use]
    pub fn new(units: impl IntoIterator<Item = UnitId>) -> Self {
        Self {
            units: units
                .into_iter()
                .map(|unit| (unit, UnitMapping::default()))
                .collect(),
            visited: None,
        }
    }

    #[must_use]
    pub fn unit(&self, unit: UnitId) -> Option<&UnitMapping> {
        self.units.get(&unit)
    }

    /// The next unit with something to map after the last visited one, wrapping to the first, with
    /// its offset. `ceilings` holds each known unit's C; unknown units still descend and bracket.
    #[must_use]
    pub fn next_target(&self, ceilings: &BTreeMap<UnitId, TargetOffset>) -> Option<MappingTarget> {
        let after = match self.visited {
            Some(visited) => self
                .units
                .range((Bound::Excluded(visited), Bound::Unbounded)),
            None => self.units.range::<UnitId, _>(..),
        };
        let wrapped = self
            .visited
            .into_iter()
            .flat_map(|visited| self.units.range(..=visited));
        after.chain(wrapped).find_map(|(&unit, mapping)| {
            let ceiling = ceilings.get(&unit).copied().unwrap_or(TargetOffset::ZERO);
            mapping
                .target(ceiling)
                .map(|(offset, kind)| MappingTarget { unit, offset, kind })
        })
    }

    /// Fold a finished step of `unit` and move the round-robin past it.
    pub fn finish_step(&mut self, unit: UnitId, result: StepResult) {
        if let Some(mapping) = self.units.get_mut(&unit) {
            mapping.finish(result);
            self.visited = Some(unit);
        }
    }

    /// Fold an isolated observation of a mapping or reproduction trial.
    pub fn observe(&mut self, observation: IsolatedObservation) {
        if let Some(mapping) = self.units.get_mut(&observation.unit) {
            mapping.observe(observation.offset, observation.result);
        }
    }
}
