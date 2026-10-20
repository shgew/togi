use std::collections::BTreeMap;
use std::num::NonZeroU32;

use togi_journal::Seq;
use togi_machine::{CcdId, CoreSet, PerCore};

use crate::Profile;

/// One derived partial of a CCD's chain: the loaded set the predecessor's top group left, the
/// profile it was derived at, and the event that recorded it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Node {
    pub(crate) part: NonZeroU32,
    pub(crate) link: Link,
    pub(crate) profile: Profile,
    pub(crate) seq: Seq,
}

/// What a chain node derives: the next partial's loaded cores, or the end of the chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Link {
    Partial(CoreSet),
    End,
}

impl Link {
    /// The link a `checking.chain` records, whose empty set ends the chain.
    pub(crate) fn recorded(cores: CoreSet) -> Self {
        if cores.is_empty() {
            Self::End
        } else {
            Self::Partial(cores)
        }
    }

    /// The loaded set a `checking.chain` records for the link.
    pub(crate) fn recorded_cores(self) -> CoreSet {
        match self {
            Self::Partial(cores) => cores,
            Self::End => CoreSet::default(),
        }
    }
}

/// A CCD's chain as recorded, and how much of it stands at the checking profile: the prefix whose
/// nodes were derived there.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Chain {
    nodes: Vec<Node>,
    current: usize,
}

impl Chain {
    pub(crate) fn current(&self) -> &[Node] {
        &self.nodes[..self.current]
    }

    /// The node recorded for chain position `index`, whether or not it still stands.
    pub(crate) fn recorded(&self, index: usize) -> Option<&Node> {
        self.nodes.get(index)
    }

    pub(crate) fn complete(&self) -> bool {
        self.current()
            .last()
            .is_some_and(|node| node.link == Link::End)
    }

    pub(crate) fn record(&mut self, node: Node) {
        match self.nodes.iter().position(|old| old.part == node.part) {
            None => self.nodes.push(node),
            Some(at) if node.link == Link::End => {
                self.nodes.truncate(at);
                self.nodes.push(node);
            }
            Some(at) => self.nodes[at] = node,
        }
    }

    pub(crate) fn retarget(&mut self, profile: &Profile) {
        self.current = self
            .nodes
            .iter()
            .take_while(|node| &node.profile == profile)
            .count();
    }
}

/// An R7 occurrence of an open cycle that has started: its `checking.step` event and the chain of
/// partials derived for each CCD.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct R7Step {
    pub(crate) start: Seq,
    chains: BTreeMap<CcdId, Chain>,
}

impl R7Step {
    pub(crate) fn new(start: Seq) -> Self {
        Self {
            start,
            chains: BTreeMap::new(),
        }
    }

    pub(crate) fn chain(&self, ccd: CcdId) -> Option<&Chain> {
        self.chains.get(&ccd)
    }

    pub(crate) fn current(&self, ccd: CcdId) -> &[Node] {
        self.chain(ccd).map_or(&[], Chain::current)
    }

    pub(crate) fn record(&mut self, ccd: CcdId, node: Node, profile: Option<&Profile>) {
        let chain = self.chains.entry(ccd).or_default();
        chain.record(node);
        if let Some(profile) = profile {
            chain.retarget(profile);
        }
    }

    pub(crate) fn retarget(&mut self, profile: &Profile) {
        for chain in self.chains.values_mut() {
            chain.retarget(profile);
        }
    }

    pub(crate) fn complete(&self, ccds: impl IntoIterator<Item = CcdId>) -> bool {
        ccds.into_iter()
            .all(|ccd| self.chain(ccd).is_some_and(Chain::complete))
    }
}

/// What ordered an [`HonestChain`]'s partials.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub enum PartialBasis {
    /// Every loaded core had a finite measured request. Successive top request groups, ties within
    /// [`togi_requests::TIE_V`] together, are idled.
    Measured,
    /// The `missing` loaded cores had no finite measured request, so no request order exists. The
    /// chain idles one core per partial, highest core id first. Offsets never stand in for
    /// requests, and measured requests of the other cores are not mixed in.
    SubsetFallback { missing: CoreSet },
}

/// Why a loaded set derives no partial with at least two loaded cores.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum UncoveredReason {
    /// Fewer than three cores are loaded: idling any core leaves fewer than two.
    TooFewLoaded,
    /// The measured top request group, `top`, leaves fewer than two loaded cores.
    TopGroupLeavesTooFew { top: CoreSet },
}

/// The partial loads of one CCD's loaded set: a chain of partials, each with at least two loaded
/// cores and each a strict subset of the one before, or an explicit uncovered result. An uncovered
/// set is never partial coverage: no partial ran for it, and it says why.
///
/// This is the seam for checking roles (#1217). The current checking scheduler does not call it:
/// it still derives one link at a time from the request order, which lets offsets
/// stand in for missing requests and records an empty link as the end of the chain
/// (`docs/spec/workloads.md`).
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum HonestChain {
    Partials {
        loaded: CoreSet,
        basis: PartialBasis,
        partials: Vec<CoreSet>,
    },
    Uncovered {
        loaded: CoreSet,
        basis: PartialBasis,
        reason: UncoveredReason,
    },
}

impl HonestChain {
    /// The chain for `loaded` from `requests`, the measured request in volts of the load
    /// `loaded`. Requests of unloaded cores are ignored; a non-finite request counts as missing.
    /// When every loaded core has a request the chain is [`PartialBasis::Measured`], otherwise
    /// [`PartialBasis::SubsetFallback`].
    #[must_use]
    pub fn derive(loaded: CoreSet, requests: &PerCore<f64>) -> Self {
        let mut measured = PerCore::default();
        let mut missing = CoreSet::default();
        for core in loaded {
            match requests.get(core).filter(|request| request.is_finite()) {
                Some(request) => measured.set(core, request),
                None => {
                    missing.insert(core);
                }
            }
        }
        let basis = if missing.is_empty() {
            PartialBasis::Measured
        } else {
            PartialBasis::SubsetFallback { missing }
        };
        let uncovered = |reason| Self::Uncovered {
            loaded,
            basis,
            reason,
        };
        if loaded.len() < 3 {
            return uncovered(UncoveredReason::TooFewLoaded);
        }
        let mut partials = Vec::new();
        let mut remaining = loaded;
        match basis {
            PartialBasis::Measured => {
                let groups = togi_requests::groups(&measured);
                for group in &groups {
                    remaining = remaining.iter().filter(|&core| !group.contains(core)).collect();
                    if remaining.len() < 2 {
                        break;
                    }
                    partials.push(remaining);
                }
                if partials.is_empty() {
                    let top = groups.first().copied().unwrap_or_default();
                    return uncovered(UncoveredReason::TopGroupLeavesTooFew { top });
                }
            }
            PartialBasis::SubsetFallback { .. } => {
                while let Some(highest) = remaining.last() {
                    remaining.remove(highest);
                    if remaining.len() < 2 {
                        break;
                    }
                    partials.push(remaining);
                }
            }
        }
        Self::Partials {
            loaded,
            basis,
            partials,
        }
    }

    /// The loaded set the chain was derived for.
    #[must_use]
    pub fn loaded(&self) -> CoreSet {
        match self {
            Self::Partials { loaded, .. } | Self::Uncovered { loaded, .. } => *loaded,
        }
    }

    #[must_use]
    pub fn basis(&self) -> PartialBasis {
        match self {
            Self::Partials { basis, .. } | Self::Uncovered { basis, .. } => *basis,
        }
    }

    /// The partials in order, empty when uncovered.
    #[must_use]
    pub fn partials(&self) -> &[CoreSet] {
        match self {
            Self::Partials { partials, .. } => partials,
            Self::Uncovered { .. } => &[],
        }
    }

    /// Whether a nonempty chain of partial loads exists. An empty list never supplies coverage,
    /// including a list decoded from a caller's serialized plan.
    #[must_use]
    pub fn is_coverage(&self) -> bool {
        !self.partials().is_empty()
    }
}
