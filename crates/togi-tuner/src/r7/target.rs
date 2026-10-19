use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU16;

use togi_machine::{CcdId, CoreId, PerCore, Regime, TargetOffset};
use togi_requests::{VOLTS_PER_COUNT, counts, top};

use togi_journal::Seq;
use togi_machine::WorkloadId;

use crate::{BackendId, ClassKey, Depth, Evidence, FailurePoint, LoadKey, Profile, Topology};

const NAMED_CLOCK_TOLERANCE_MHZ: i64 = 25;

/// Which passes may set the target, and what a candidate's request must beat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Basis {
    /// An unattributed failure: only passes whose top request is strictly higher than the
    /// failing one, `failing_top`, or the original rail when the move steps down.
    Unattributed { failing_top: f64 },
    /// A named core: its CCD clock must be within the candidate tolerance of the failure's
    /// current or last same-load measured clock. No clock means no qualifying target.
    Named { failing_mhz: Option<i64> },
}

impl Basis {
    fn admits(self, request: f64, mhz: Option<i64>) -> bool {
        match self {
            Self::Unattributed { failing_top } => request > failing_top,
            Self::Named { failing_mhz } => {
                failing_mhz.zip(mhz).is_some_and(|(failing, passing)| {
                    passing >= failing.saturating_sub(NAMED_CLOCK_TOLERANCE_MHZ)
                })
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

/// The newest failure of `class` at a profile `profile` is not deeper than: a pass of that
/// profile is valid only if it is newer.
fn failure_floor(evidence: &Evidence, class: ClassKey, profile: &Profile) -> Option<Seq> {
    evidence.newest_failure(class, profile, None).ok().flatten()
}

/// Counts passes at one failure floor and retains the earliest witnesses up to the supported
/// evidence threshold, so a changed configuration can cite earlier passes without a ledger scan.
#[derive(Clone, Debug, PartialEq)]
struct Segment {
    floor: Option<Seq>,
    count: u32,
    first: std::sync::Arc<Vec<Seq>>,
}

/// The passes one request and clock summary stands for, oldest segment first.
#[derive(Clone, Debug, Default, PartialEq)]
struct Entry {
    segments: Vec<Segment>,
}

impl Entry {
    fn add(&mut self, seq: Seq, floor: Option<Seq>) {
        match self.segments.last_mut() {
            Some(last) if last.floor == floor => {
                last.count = crate::numbers::saturating_evidence_count(u64::from(last.count) + 1);
                if last.first.len() < usize::from(togi_config::TrialCount::MAX) {
                    crate::evidence::unique(&mut last.first).push(seq);
                }
            }
            _ => self.segments.push(Segment {
                floor,
                count: 1,
                first: std::sync::Arc::new(vec![seq]),
            }),
        }
    }

    /// The count and first `n` seqs of the passes newer than `failure`, none when no pass is.
    fn valid(&self, failure: Option<Seq>, n: usize) -> Option<(u64, Vec<Seq>)> {
        let mut count = 0_u64;
        let mut first: Vec<Seq> = Vec::new();
        for segment in self
            .segments
            .iter()
            .filter(|segment| segment.floor >= failure)
        {
            count += u64::from(segment.count);
            let room = n.saturating_sub(first.len());
            first.extend(segment.first.iter().take(room));
        }
        (count > 0).then_some((count, first))
    }
}

/// A CCD, a request's exact bits and, for a named target, that CCD's clock: all one query reads
/// of a pass.
type Key = (CcdId, u64, Option<i64>);

type GroupId = u32;

/// The passes of one class, backend and profile, reduced to each CCD's highest request,
/// without clocks for unattributed targets and with clocks for named targets.
#[derive(Clone, Debug, PartialEq)]
struct Group {
    class: ClassKey,
    backend: BackendId,
    profile: Profile,
    ccd_top: BTreeMap<Key, Entry>,
    load_top: BTreeMap<Key, Entry>,
}

#[derive(Clone, Debug, PartialEq)]
struct Candidate {
    volts: f64,
    lowest: Seq,
    count: u64,
    first: Vec<Seq>,
}

impl Candidate {
    fn add(&mut self, request: f64, count: u64, first: &[Seq], n: usize) {
        let earliest = first[0];
        if request
            .total_cmp(&self.volts)
            .then(earliest.cmp(&self.lowest))
            == std::cmp::Ordering::Less
        {
            self.volts = request;
            self.lowest = earliest;
        }
        self.count += count;
        self.first.extend_from_slice(first);
        self.first.sort_unstable();
        self.first.truncate(n);
    }

    fn cited(&self, n: usize) -> Vec<Seq> {
        let mut cited = self.first[..n].to_vec();
        if !cited.contains(&self.lowest) {
            cited[n - 1] = self.lowest;
            cited.sort_unstable();
        }
        cited
    }
}

/// The passes a voltage target may come from. Passes are grouped by class, backend and profile,
/// and within a group by the request and clock summary a query reads, so its size follows the
/// distinct summaries and the distinct failure floors they met, not the passes. A pass that a
/// failure of its class rules out is not deleted: queries compare each segment's floor with the
/// ledger's newest live failure, so a failure the ledger drops (a reset of the core it named)
/// gives its passes back. A reset of a loaded core discards the groups of the loads it is in.
/// Passes that carry no request lane never enter.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct R7Targets {
    next: GroupId,
    groups: BTreeMap<GroupId, Group>,
    shapes: HashMap<(ClassKey, BackendId, Profile), GroupId>,
    by_load: HashMap<LoadKey, BTreeSet<GroupId>>,
    by_core: HashMap<WorkloadId, PerCore<Option<BTreeSet<GroupId>>>>,
    #[cfg(test)]
    visits: crate::evidence::Visits,
}

impl R7Targets {
    /// The entries stored.
    #[cfg(test)]
    pub(crate) fn entries(&self) -> usize {
        self.groups
            .values()
            .map(|group| group.ccd_top.len() + group.load_top.len())
            .sum()
    }

    /// The segments stored.
    #[cfg(test)]
    pub(crate) fn segments(&self) -> usize {
        self.groups
            .values()
            .flat_map(|group| group.ccd_top.values().chain(group.load_top.values()))
            .map(|entry| entry.segments.len())
            .sum()
    }

    /// How many witness lists share their storage with the same list of `other`, and how many
    /// there are.
    #[cfg(test)]
    pub(crate) fn shared_with(&self, other: &Self) -> (usize, usize) {
        let mut shared = 0;
        let mut total = 0;
        for (group, twin) in self
            .groups
            .iter()
            .filter_map(|(id, group)| other.groups.get(id).map(|twin| (group, twin)))
        {
            for (entries, others) in [
                (&group.ccd_top, &twin.ccd_top),
                (&group.load_top, &twin.load_top),
            ] {
                for (entry, other) in entries
                    .iter()
                    .filter_map(|(key, entry)| others.get(key).map(|other| (entry, other)))
                {
                    for (segment, twin) in entry.segments.iter().zip(&other.segments) {
                        total += 1;
                        shared += usize::from(std::sync::Arc::ptr_eq(&segment.first, &twin.first));
                    }
                }
            }
        }
        (shared, total)
    }

    /// The groups stored.
    #[cfg(test)]
    pub(crate) fn groups(&self) -> usize {
        self.groups.len()
    }

    #[cfg(test)]
    pub(crate) fn witnesses(&self) -> usize {
        self.groups
            .values()
            .flat_map(|group| group.ccd_top.values().chain(group.load_top.values()))
            .flat_map(|entry| &entry.segments)
            .map(|segment| segment.first.len())
            .sum()
    }

    #[cfg(test)]
    pub(crate) fn take_visits(&self) -> u64 {
        self.visits.0.swap(0, std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    fn visit(&self) {
        self.visits
            .0
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(not(test))]
    #[expect(clippy::unused_self, reason = "the test build counts visits here")]
    fn visit(&self) {}

    /// Indexes a passing R7 trial that measured requests, remembering the failure floor the
    /// ledger holds for its profile now; any other pass is ignored. Returns whether it was indexed.
    pub(crate) fn insert(
        &mut self,
        topology: &Topology,
        evidence: &Evidence,
        pass: &PassRecord,
    ) -> bool {
        if pass.class.regime() != Regime::R7 {
            return false;
        }
        let Some(load_top) = top(&pass.requests) else {
            return false;
        };
        let floor = failure_floor(evidence, pass.class, &pass.profile);
        let shape = (pass.class, pass.backend, pass.profile);
        let id = match self.shapes.get(&shape) {
            Some(&id) => id,
            None => self.open_group(pass),
        };
        let Some(group) = self.groups.get_mut(&id) else {
            return false;
        };
        let mut ccd_tops: BTreeMap<CcdId, f64> = BTreeMap::new();
        for (core, request) in pass.requests.iter() {
            if let Some(ccd) = topology.ccd_of(core) {
                ccd_tops
                    .entry(ccd)
                    .and_modify(|highest| *highest = highest.max(request))
                    .or_insert(request);
            }
        }
        for (&ccd, &highest) in &ccd_tops {
            let key = (ccd, highest.to_bits(), None);
            group.ccd_top.entry(key).or_default().add(pass.seq, floor);
        }
        let loaded: BTreeSet<CcdId> = pass
            .class
            .loaded()
            .iter()
            .filter_map(|core| topology.ccd_of(core))
            .collect();
        for ccd in loaded {
            let key = (
                ccd,
                ccd_tops.get(&ccd).copied().unwrap_or(load_top).to_bits(),
                pass.ccd_mhz.get(&ccd).copied(),
            );
            group.load_top.entry(key).or_default().add(pass.seq, floor);
        }
        true
    }

    fn open_group(&mut self, pass: &PassRecord) -> GroupId {
        let id = self.next;
        self.next += 1;
        let load = pass.class.load();
        self.by_load.entry(load).or_default().insert(id);
        for core in load.loaded {
            self.by_core
                .entry(load.workload)
                .or_default()
                .get_or_insert_with(core, BTreeSet::new)
                .insert(id);
        }
        self.shapes
            .insert((pass.class, pass.backend, pass.profile), id);
        self.groups.insert(
            id,
            Group {
                class: pass.class,
                backend: pass.backend,
                profile: pass.profile,
                ccd_top: BTreeMap::new(),
                load_top: BTreeMap::new(),
            },
        );
        id
    }

    fn drop_group(&mut self, id: GroupId) {
        let Some(group) = self.groups.remove(&id) else {
            return;
        };
        self.shapes
            .remove(&(group.class, group.backend, group.profile));
        let load = group.class.load();
        if let Some(ids) = self.by_load.get_mut(&load) {
            ids.remove(&id);
            if ids.is_empty() {
                self.by_load.remove(&load);
            }
        }
        for core in load.loaded {
            if let Some(cores) = self.by_core.get_mut(&load.workload)
                && let Some(ids) = cores.get_mut(core).and_then(Option::as_mut)
            {
                ids.remove(&id);
                if ids.is_empty() {
                    cores.set(core, None);
                }
            }
        }
    }

    /// Removes the passes whose load includes `core`.
    pub(crate) fn reset_core(&mut self, core: CoreId) {
        let doomed: Vec<GroupId> = self
            .groups
            .iter()
            .filter(|(_, group)| group.class.loaded().contains(core))
            .map(|(&id, _)| id)
            .collect();
        for id in doomed {
            self.drop_group(id);
        }
    }

    /// The lowest request among the admitted entries of `ccd` that reaches `n` valid passes at
    /// one profile, ties to the profile with the older pass. A pass is valid when no failure the
    /// ledger holds for its class and profile is newer.
    fn query(
        &self,
        ask: Ask<'_>,
        ids: &BTreeSet<GroupId>,
        ccd: CcdId,
        basis: Basis,
        entries: fn(&Group) -> &BTreeMap<Key, Entry>,
    ) -> Option<Target> {
        let needed = u64::from(ask.n.get());
        let n = usize::from(ask.n.get());
        let mut index: HashMap<(LoadKey, Profile), usize> = HashMap::new();
        let mut candidates: Vec<Candidate> = Vec::new();
        for group in ids
            .iter()
            .filter_map(|id| self.groups.get(id))
            .filter(|group| group.backend == ask.backend)
            .filter(|group| {
                !matches!(basis, Basis::Named { .. })
                    || group
                        .class
                        .loaded()
                        .iter()
                        .all(|core| ask.topology.ccd_of(core) == Some(ccd))
            })
        {
            self.visit();
            let failure = failure_floor(ask.evidence, group.class, &group.profile);
            let span = (ccd, 0, None)..=(ccd, u64::MAX, Some(i64::MAX));
            for (&(_, bits, mhz), entry) in entries(group).range(span) {
                self.visit();
                let request = f64::from_bits(bits);
                if !basis.admits(request, mhz) {
                    continue;
                }
                #[cfg(test)]
                for _ in &entry.segments {
                    self.visit();
                }
                let Some((count, first)) = entry.valid(failure, n) else {
                    continue;
                };
                let shape = (group.class.load(), group.profile);
                if let Some(&at) = index.get(&shape) {
                    candidates[at].add(request, count, &first, n);
                } else {
                    index.insert(shape, candidates.len());
                    candidates.push(Candidate {
                        volts: request,
                        lowest: first[0],
                        count,
                        first,
                    });
                }
            }
        }
        candidates
            .iter()
            .filter(|candidate| candidate.count >= needed)
            .min_by(|a, b| {
                a.volts
                    .total_cmp(&b.volts)
                    .then(a.first[0].cmp(&b.first[0]))
            })
            .map(|candidate| Target {
                volts: candidate.volts,
                cited: candidate.cited(n),
            })
    }

    /// The target for an unattributed failure of `load` that moves `moved`: its top request is
    /// the CCD's highest among the loaded cores, and only passes of exactly that load count.
    pub(crate) fn unattributed(
        &self,
        ask: Ask<'_>,
        load: LoadKey,
        moved: CoreId,
        failing_top: f64,
    ) -> Option<Target> {
        let ccd = ask.topology.ccd_of(moved)?;
        let ids = self.by_load.get(&load)?;
        self.query(
            ask,
            ids,
            ccd,
            Basis::Unattributed { failing_top },
            |group| &group.ccd_top,
        )
    }

    /// The target for a failure naming `core`: only loads containing it and wholly on its
    /// CCD qualify. Durations can combine, but different loads and profiles cannot.
    pub(crate) fn named(
        &self,
        ask: Ask<'_>,
        workload: WorkloadId,
        core: CoreId,
        failing_mhz: Option<i64>,
    ) -> Option<Target> {
        let ccd = ask.topology.ccd_of(core)?;
        let ids = self.by_core.get(&workload)?.get_ref(core)?.as_ref()?;
        self.query(ask, ids, ccd, Basis::Named { failing_mhz }, |group| {
            &group.load_top
        })
    }
}

/// What a target query reads besides the index: the machine, the ledger that says which passes
/// still count, the backend whose passes count and the passes a target needs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ask<'a> {
    pub(crate) topology: &'a Topology,
    pub(crate) evidence: &'a Evidence,
    pub(crate) backend: BackendId,
    pub(crate) n: NonZeroU16,
}

const COUNT_ROUNDING_TOLERANCE: f64 = 1e-9;

/// Earlier crash summaries need not represent the requests of a full passing trial.
pub(crate) const CRASH_TARGET_MIN_S: u32 = 60;

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
    floor.saturating_add(1).max(1)
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
    let failure_point = FailurePoint::new(applied)
        .merge(prior_failure_point.map(FailurePoint::new))
        .offset();
    let counts = saturating_backoff_counts(counts);
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

fn saturating_backoff_counts(counts: i64) -> u8 {
    match u8::try_from(counts.clamp(0, i64::from(u8::MAX))) {
        Ok(counts) => counts,
        Err(_) => unreachable!("backoff counts were clamped to the u8 range"),
    }
}
