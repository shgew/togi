use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU16;

use togi_machine::{CcdId, CoreId, PerCore, Regime, TargetOffset};
use togi_requests::{VOLTS_PER_COUNT, counts, top};

use togi_journal::Seq;
use togi_machine::WorkloadId;

use crate::{BackendId, ClassKey, Depth, LoadKey, Profile, Topology};

/// A passing R7 trial seen as a candidate for the voltage a backoff aims at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TargetPass {
    pub(crate) seq: Seq,
    pub(crate) profile: Profile,
    pub(crate) top_request: f64,
    /// The pass's median clock on the moved core's CCD, when it recorded one.
    pub(crate) ccd_mhz: Option<i64>,
}

/// Which passes may set the target, and what a candidate's request must beat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Basis {
    /// An unattributed failure: only passes whose top request is strictly higher than the
    /// failing one, `failing_top`, or the original rail when the move steps down.
    Unattributed { failing_top: f64 },
    /// A named core: passes whose clock reaches the failing trial's, `failing_mhz`, with no
    /// clock filter when the failing trial has none. The target may equal or undercut the
    /// failing request.
    Named { failing_mhz: Option<i64> },
}

impl Basis {
    fn admits(self, pass: &TargetPass) -> bool {
        match self {
            Self::Unattributed { failing_top } => pass.top_request > failing_top,
            Self::Named { failing_mhz } => {
                failing_mhz.is_none_or(|failing| pass.ccd_mhz.unwrap_or(0) >= failing)
            }
        }
    }
}

/// The voltage a backoff aims at and the passes it cites.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Target {
    pub(crate) volts: f64,
    /// The first `n` qualifying passes of the chosen profile, ascending, with the pass that set
    /// the voltage in place of the `n`th when it is not among them.
    pub(crate) cited: Vec<Seq>,
}

struct Candidate {
    volts: f64,
    lowest: Seq,
    seqs: Vec<Seq>,
}

impl Candidate {
    fn add(&mut self, pass: &TargetPass) {
        if pass
            .top_request
            .total_cmp(&self.volts)
            .then(pass.seq.cmp(&self.lowest))
            == Ordering::Less
        {
            self.volts = pass.top_request;
            self.lowest = pass.seq;
        }
        self.seqs.push(pass.seq);
    }

    fn cited(&self, n: usize) -> Vec<Seq> {
        let mut cited = self.seqs[..n].to_vec();
        if !cited.contains(&self.lowest) {
            cited[n - 1] = self.lowest;
            cited.sort_unstable();
        }
        cited
    }
}

/// The lowest top request of any profile with `n` admitted passes: the passes are grouped by
/// profile, a profile's voltage is the lowest request among its passes, equal voltages go to the
/// profile whose earliest pass is older. Voltages compare exactly.
pub(crate) fn voltage_target(
    passes: impl IntoIterator<Item = TargetPass>,
    basis: Basis,
    n: NonZeroU16,
) -> Option<Target> {
    let mut admitted: Vec<TargetPass> = passes
        .into_iter()
        .filter(|pass| basis.admits(pass))
        .collect();
    admitted.sort_unstable_by_key(|pass| pass.seq);
    let mut index: HashMap<Profile, usize> = HashMap::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for pass in &admitted {
        let at = *index.entry(pass.profile).or_insert_with(|| {
            candidates.push(Candidate {
                volts: pass.top_request,
                lowest: pass.seq,
                seqs: Vec::new(),
            });
            candidates.len() - 1
        });
        candidates[at].add(pass);
    }
    let n = usize::from(n.get());
    candidates
        .iter()
        .filter(|candidate| candidate.seqs.len() >= n)
        .min_by(|a, b| a.volts.total_cmp(&b.volts).then(a.seqs[0].cmp(&b.seqs[0])))
        .map(|candidate| Target {
            volts: candidate.volts,
            cited: candidate.cited(n),
        })
}

/// What a trial must tell the index about a passing R7 trial.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PassRecord {
    pub(crate) seq: Seq,
    pub(crate) class: ClassKey,
    /// The backend identity the pass ran under.
    pub(crate) backend: BackendId,
    pub(crate) profile: Profile,
    pub(crate) requests: PerCore<f64>,
    pub(crate) ccd_mhz: BTreeMap<CcdId, i64>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Shape {
    class: ClassKey,
    backend: BackendId,
    profile: Profile,
    requests: Vec<(CoreId, u64)>,
    ccd_mhz: Vec<(CcdId, i64)>,
}

impl Shape {
    fn of(pass: &PassRecord) -> Self {
        Self {
            class: pass.class,
            backend: pass.backend,
            profile: pass.profile,
            requests: pass
                .requests
                .iter()
                .map(|(core, volts)| (core, volts.to_bits()))
                .collect(),
            ccd_mhz: pass.ccd_mhz.iter().map(|(&ccd, &mhz)| (ccd, mhz)).collect(),
        }
    }
}

/// The passes with one shape: the same class, backend, profile, requests and clocks. Only their
/// sequences differ, ascending.
#[derive(Clone, Debug, PartialEq)]
struct Group {
    record: PassRecord,
    seqs: Vec<Seq>,
}

type GroupId = u32;

/// The passes a voltage target may come from, grouped by shape and indexed by load and by loaded
/// core. A target query visits the groups of its load, each contributing at most `n` passes, never
/// the passes themselves. A pass is in the index while it counts as evidence: the fold removes it
/// when a later failure of its class invalidates it ([`R7Targets::invalidate`]) or a reset
/// discards it. Passes that carry no request lane never enter.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct R7Targets {
    next: GroupId,
    groups: BTreeMap<GroupId, Group>,
    shapes: HashMap<Shape, GroupId>,
    by_load: HashMap<LoadKey, BTreeSet<GroupId>>,
    by_core: HashMap<(WorkloadId, CoreId), BTreeSet<GroupId>>,
}

impl R7Targets {
    /// The passes indexed.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.groups.values().map(|group| group.seqs.len()).sum()
    }

    /// The distinct shapes indexed.
    #[cfg(test)]
    pub(crate) fn shapes(&self) -> usize {
        self.groups.len()
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, seq: Seq) -> bool {
        self.groups
            .values()
            .any(|group| group.seqs.binary_search(&seq).is_ok())
    }

    /// Indexes a passing R7 trial that measured requests; any other pass is ignored. Returns
    /// whether it was indexed.
    pub(crate) fn insert(&mut self, pass: PassRecord) -> bool {
        if pass.class.regime() != Regime::R7 || pass.requests.is_empty() {
            return false;
        }
        let shape = Shape::of(&pass);
        if let Some(&id) = self.shapes.get(&shape) {
            if let Some(group) = self.groups.get_mut(&id) {
                let at = group.seqs.partition_point(|&seq| seq < pass.seq);
                group.seqs.insert(at, pass.seq);
            }
            return true;
        }
        let id = self.next;
        self.next += 1;
        let load = pass.class.load();
        self.by_load.entry(load).or_default().insert(id);
        for core in load.loaded {
            self.by_core
                .entry((load.workload, core))
                .or_default()
                .insert(id);
        }
        self.shapes.insert(shape, id);
        self.groups.insert(
            id,
            Group {
                seqs: vec![pass.seq],
                record: pass,
            },
        );
        true
    }

    fn drop_group(&mut self, id: GroupId) {
        let Some(group) = self.groups.remove(&id) else {
            return;
        };
        let record = group.record;
        self.shapes.remove(&Shape::of(&record));
        let load = record.class.load();
        if let Some(ids) = self.by_load.get_mut(&load) {
            ids.remove(&id);
            if ids.is_empty() {
                self.by_load.remove(&load);
            }
        }
        for core in load.loaded {
            let key = (load.workload, core);
            if let Some(ids) = self.by_core.get_mut(&key) {
                ids.remove(&id);
                if ids.is_empty() {
                    self.by_core.remove(&key);
                }
            }
        }
    }

    /// Removes the passes of `class` that a failure at `failure`, folded at `seq`, rules out:
    /// those older than it at a profile `failure` is within. Newer passes of the class that
    /// cover that profile qualify again as they arrive.
    pub(crate) fn invalidate(&mut self, class: ClassKey, failure: &Profile, seq: Seq) {
        let Some(ids) = self.by_load.get(&class.load()) else {
            return;
        };
        let ruled_out: Vec<GroupId> = ids
            .iter()
            .copied()
            .filter(|id| {
                self.groups.get(id).is_some_and(|group| {
                    group.record.class == class && failure.within(&group.record.profile)
                })
            })
            .collect();
        for id in ruled_out {
            let emptied = self.groups.get_mut(&id).is_some_and(|group| {
                let older = group.seqs.partition_point(|&pass| pass < seq);
                group.seqs.drain(..older);
                group.seqs.is_empty()
            });
            if emptied {
                self.drop_group(id);
            }
        }
    }

    /// Removes the passes whose load includes `core`.
    pub(crate) fn reset_core(&mut self, core: CoreId) {
        let doomed: Vec<GroupId> = self
            .groups
            .iter()
            .filter(|(_, group)| group.record.class.loaded().contains(core))
            .map(|(&id, _)| id)
            .collect();
        for id in doomed {
            self.drop_group(id);
        }
    }

    fn candidates<'a>(
        &'a self,
        ids: &'a BTreeSet<GroupId>,
        backend: BackendId,
        n: NonZeroU16,
        pass: impl Fn(&PassRecord) -> Option<(f64, Option<i64>, Profile)> + 'a,
    ) -> impl Iterator<Item = TargetPass> + 'a {
        let n = usize::from(n.get());
        ids.iter()
            .filter_map(|id| self.groups.get(id))
            .filter(move |group| group.record.backend == backend)
            .filter_map(move |group| {
                let (top_request, ccd_mhz, profile) = pass(&group.record)?;
                Some(group.seqs.iter().take(n).map(move |&seq| TargetPass {
                    seq,
                    profile,
                    top_request,
                    ccd_mhz,
                }))
            })
            .flatten()
    }

    /// The target for an unattributed failure of `load` that moves `moved`: its top request is
    /// the CCD's highest among the loaded cores, and only passes of exactly that load count.
    pub(crate) fn unattributed(
        &self,
        topology: &Topology,
        backend: BackendId,
        load: LoadKey,
        moved: CoreId,
        failing_top: f64,
        n: NonZeroU16,
    ) -> Option<Target> {
        let ccd = topology.ccd_of(moved)?;
        let ids = self.by_load.get(&load)?;
        let passes = self.candidates(ids, backend, n, |record| {
            let on_ccd: PerCore<f64> = record
                .requests
                .iter()
                .filter(|&(core, _)| topology.ccd_of(core) == Some(ccd))
                .collect();
            Some((
                top(&on_ccd)?,
                record.ccd_mhz.get(&ccd).copied(),
                record.profile,
            ))
        });
        voltage_target(passes, Basis::Unattributed { failing_top }, n)
    }

    /// The target for a failure naming `core`: its passes under any load of `workload` that
    /// includes it, each at the highest request of its whole load.
    pub(crate) fn named(
        &self,
        topology: &Topology,
        backend: BackendId,
        workload: WorkloadId,
        core: CoreId,
        failing_mhz: Option<i64>,
        n: NonZeroU16,
    ) -> Option<Target> {
        let ccd = topology.ccd_of(core)?;
        let ids = self.by_core.get(&(workload, core))?;
        let passes = self.candidates(ids, backend, n, |record| {
            Some((
                top(&record.requests)?,
                record.ccd_mhz.get(&ccd).copied(),
                record.profile,
            ))
        });
        voltage_target(passes, Basis::Named { failing_mhz }, n)
    }
}

const COUNT_ROUNDING_TOLERANCE: f64 = 1e-9;

/// The counts shallower that lift a request from `v` strictly above `top`, and at least one:
/// `floor((top - v) / VOLTS_PER_COUNT) + 1`. A request exactly a whole number of counts below
/// `top` still needs the count that reaches it. Both must be finite.
pub(crate) fn counts_above(v: f64, top: f64) -> i64 {
    debug_assert!(v.is_finite() && top.is_finite());
    #[expect(
        clippy::cast_possible_truncation,
        reason = "`as` saturates out-of-range values and maps NaN to 0, which suits counts"
    )]
    let floor = ((top - v) / VOLTS_PER_COUNT + COUNT_ROUNDING_TOLERANCE).floor() as i64;
    (floor + 1).max(1)
}

/// The moved core's own failing request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum OwnRequest {
    Measured(f64),
    /// Its offset stood in for a request; a voltage is never invented from it.
    OffsetProxy,
}

/// How many counts shallower a backoff moves a core: enough to reach the target voltage, one
/// without a target, and one when an offset stands in for the request. A step-down past a CO-0
/// top group, with the group's `rail` request, also needs the counts that lift the core's
/// estimated request above the rail; the larger count wins.
pub(crate) fn backoff_counts(own: OwnRequest, target: Option<&Target>, rail: Option<f64>) -> i64 {
    let OwnRequest::Measured(request) = own else {
        return 1;
    };
    let toward = target.map_or(1, |target| counts(request, target.volts));
    rail.map_or(toward, |rail| toward.max(counts_above(request, rail)))
}

/// Where a backoff puts the core and the failure point it records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Move {
    /// The failed trial's offset, or the core's earlier failure point when that is shallower.
    pub(crate) failure_point: TargetOffset,
    pub(crate) to: TargetOffset,
}

/// `counts` shallower than the offset the core had in the failed trial, and at least one
/// shallower than its failure point, never past CO 0.
pub(crate) fn backoff_move(
    applied: TargetOffset,
    prior_failure_point: Option<TargetOffset>,
    counts: i64,
) -> Move {
    let failure_point = prior_failure_point.map_or(applied, |prior| {
        if prior.shallower_than(applied) {
            prior
        } else {
            applied
        }
    });
    let counts = u8::try_from(counts.clamp(0, i64::from(u8::MAX))).unwrap_or(u8::MAX);
    let by_counts = applied.shallower_by(counts);
    let past_failure = failure_point.shallower_by(1);
    Move {
        failure_point,
        to: if past_failure.shallower_than(by_counts) {
            past_failure
        } else {
            by_counts
        },
    }
}
