use std::collections::BTreeMap;
use std::num::NonZeroU32;

use togi_machine::TargetOffset;

use super::{
    Beta, Bound, Cell, Exposure, Hours, Purpose, Rate, Slope, offset_value, poisson_upper,
};

/// The edge margin: S stays this many counts shallower than the observed failing edge U.
pub const EDGE_MARGIN: u8 = 5;

/// The parameters the cap takes explicitly; [`CapLimits::new`] holds the spec values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CapLimits {
    /// The machine's incremental hazard budget λ*, divided equally among the CCDs.
    pub machine_budget: Rate,
    pub ccds: NonZeroU32,
    pub edge_margin: u8,
}

impl CapLimits {
    pub const fn new(ccds: NonZeroU32) -> Self {
        Self {
            machine_budget: Rate::MACHINE_BUDGET,
            ccds,
            edge_margin: EDGE_MARGIN,
        }
    }

    /// λ*/n: one CCD's share of the budget.
    pub fn per_ccd_budget(&self) -> Rate {
        Rate(self.machine_budget.get() / f64::from(self.ccds.get()))
    }
}

/// How the cap is being refreshed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refresh {
    /// Mapping evidence changed, or no measured slope exists yet: the recomputed S stands.
    Full,
    /// After the first measured slope, the update added only non-mapping slope evidence, which cannot
    /// deepen S: the shallower of `previous` and the recomputed S stands.
    NonMappingSlopeOnly { previous: TargetOffset },
}

/// One mapping class's bound at its deepest adequately exposed offset.
#[derive(Clone, Debug, PartialEq)]
pub struct Witness<K> {
    pub class: K,
    pub offset: TargetOffset,
    pub failures: u32,
    pub hours: Hours,
    pub bound: Bound,
}

/// Why a class is outside the cap's rate projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UncoveredReason {
    /// The class has no cells at all.
    NoObservations,
    /// The class's exposure comes only from qualification, reproduction or control purposes.
    NonMappingOnly,
    /// The class has mapping exposure, but no offset met the adequate-exposure threshold.
    MappingInadequate,
}

/// A class the cap does not cover; omission is never safety.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Uncovered<K> {
    pub class: K,
    pub reason: UncoveredReason,
}

/// What decided S.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Binding<K> {
    /// `min(U + margin, 0)` is shallower than the rate-limited offset, which is carried.
    Edge {
        rate_limited: TargetOffset,
    },
    /// The rate bound of this mapping class limits S, or limits it to the floor of the range.
    Rate {
        class: K,
    },
    NoAdequateClass,
    UnknownEdge,
    NoJustifiedDecay,
    /// A non-mapping slope update would have deepened S; the previous, shallower S stays.
    Retained {
        recomputed: TargetOffset,
    },
}

/// Cap S with its calculation witnesses, coverage-incomplete by construction.
#[derive(Clone, Debug, PartialEq)]
pub struct Cap<K> {
    pub offset: TargetOffset,
    pub binding: Binding<K>,
    pub slope: Slope,
    /// U, the shallowest observed failing offset; `None` is unknown.
    pub edge: Option<TargetOffset>,
    pub witnesses: Vec<Witness<K>>,
    pub uncovered: Vec<Uncovered<K>>,
}

/// `b(s) = b(x) * exp(beta*(x-s))`, projected in both directions from the witness's offset x.
pub fn project<K>(witness: &Witness<K>, beta: Beta, offset: TargetOffset) -> Bound {
    let distance = offset_value(witness.offset) - offset_value(offset);
    witness.bound.scaled((beta.get() * distance).exp())
}

/// Computes S for one CCD from every class's cells, of any purpose.
///
/// A mapping class is covered when some offset has adequately exposed mapping cells; its witness is the
/// deepest such offset. Every other class is reported uncovered with its reason. `edge` is U; `slope`
/// comes from [`Slope::measure`].
pub fn cap<K: Clone + Ord>(
    classes: &BTreeMap<K, Vec<Cell>>,
    slope: Slope,
    edge: Option<TargetOffset>,
    refresh: Refresh,
    limits: &CapLimits,
) -> Cap<K> {
    let mut witnesses = Vec::new();
    let mut uncovered = Vec::new();
    for (class, cells) in classes {
        match witness(class, cells) {
            Ok(witness) => witnesses.push(witness),
            Err(reason) => uncovered.push(Uncovered {
                class: class.clone(),
                reason,
            }),
        }
    }
    let (recomputed, binding) = decide(&witnesses, slope, edge, limits);
    let (offset, binding) = match refresh {
        Refresh::NonMappingSlopeOnly { previous } if previous > recomputed => {
            (previous, Binding::Retained { recomputed })
        }
        Refresh::Full | Refresh::NonMappingSlopeOnly { .. } => (recomputed, binding),
    };
    Cap {
        offset,
        binding,
        slope,
        edge,
        witnesses,
        uncovered,
    }
}

/// Mapping failures, exposure and adequacy merged at one offset.
struct Merged {
    failures: u32,
    hours: Hours,
    adequate: bool,
}

/// Failures at one offset add; the count stops at the largest representable value.
fn saturating_failures(left: u32, right: u32) -> u32 {
    left.saturating_add(right)
}

fn witness<K: Clone>(class: &K, cells: &[Cell]) -> Result<Witness<K>, UncoveredReason> {
    if cells.is_empty() {
        return Err(UncoveredReason::NoObservations);
    }
    let mut merged: BTreeMap<TargetOffset, Merged> = BTreeMap::new();
    for cell in cells.iter().filter(|cell| cell.purpose == Purpose::Mapping) {
        let entry = merged.entry(cell.offset).or_insert(Merged {
            failures: 0,
            hours: Hours::ZERO,
            adequate: false,
        });
        entry.failures = saturating_failures(entry.failures, cell.failures);
        entry.hours = entry.hours.saturating_add(cell.hours);
        entry.adequate |= match cell.exposure {
            Exposure::Adequate => true,
            Exposure::Partial => false,
        };
    }
    if merged.is_empty() {
        return Err(UncoveredReason::NonMappingOnly);
    }
    // Ascending offsets put the deepest first.
    let Some((&offset, deepest)) = merged.iter().find(|(_, cell)| cell.adequate) else {
        return Err(UncoveredReason::MappingInadequate);
    };
    Ok(Witness {
        class: class.clone(),
        offset,
        failures: deepest.failures,
        hours: deepest.hours,
        bound: poisson_upper(deepest.failures, deepest.hours),
    })
}

fn decide<K: Clone>(
    witnesses: &[Witness<K>],
    slope: Slope,
    edge: Option<TargetOffset>,
    limits: &CapLimits,
) -> (TargetOffset, Binding<K>) {
    if witnesses.is_empty() {
        return (TargetOffset::ZERO, Binding::NoAdequateClass);
    }
    let Some(edge) = edge else {
        return (TargetOffset::ZERO, Binding::UnknownEdge);
    };
    let Some(beta) = slope.beta() else {
        return (TargetOffset::ZERO, Binding::NoJustifiedDecay);
    };
    let rate_limited = rate_limited(witnesses, beta, limits.per_ccd_budget());
    let edge_limit = TargetOffset::clamped((edge.get() + i64::from(limits.edge_margin)).min(0));
    if edge_limit > rate_limited {
        (edge_limit, Binding::Edge { rate_limited })
    } else {
        (
            rate_limited,
            Binding::Rate {
                class: limiting_class(witnesses, beta, rate_limited),
            },
        )
    }
}

/// The deepest integer in [-50, 0) where every class's projection is within `budget`; CO 0 when none is.
fn rate_limited<K>(witnesses: &[Witness<K>], beta: Beta, budget: Rate) -> TargetOffset {
    (TargetOffset::clamped(i64::MIN).get()..0)
        .map(TargetOffset::clamped)
        .find(|&offset| {
            witnesses
                .iter()
                .all(|witness| project(witness, beta, offset).within(budget))
        })
        .unwrap_or(TargetOffset::ZERO)
}

/// The class with the largest projection at the first offset the rate rule rejected (or at the deepest
/// offset when it rejected none): the one that kept S from going deeper. Ties go to the first class.
fn limiting_class<K: Clone>(witnesses: &[Witness<K>], beta: Beta, rate_limited: TargetOffset) -> K {
    let probe = TargetOffset::clamped(rate_limited.get().saturating_sub(1));
    let mut limiting = &witnesses[0];
    let mut largest = project(limiting, beta, probe);
    for witness in &witnesses[1..] {
        let bound = project(witness, beta, probe);
        if bound > largest {
            limiting = witness;
            largest = bound;
        }
    }
    limiting.class.clone()
}
