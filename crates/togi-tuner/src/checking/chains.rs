use std::collections::BTreeMap;
use std::num::NonZeroU32;

use togi_journal::Seq;
use togi_machine::{CcdId, CoreSet};

use crate::Profile;

/// One derived partial of a CCD's chain: the loaded set the predecessor's top group left, the
/// profile it was derived at, and the event that recorded it. An empty set ends the chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Node {
    pub(crate) part: NonZeroU32,
    pub(crate) cores: CoreSet,
    pub(crate) profile: Profile,
    pub(crate) seq: Seq,
}

/// A CCD's chain as recorded, and how much of it stands at the checking profile: the prefix whose
/// nodes were derived there.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
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
            .is_some_and(|node| node.cores.is_empty())
    }

    pub(crate) fn record(&mut self, node: Node) {
        match self.nodes.iter().position(|old| old.part == node.part) {
            None => self.nodes.push(node),
            Some(at) if node.cores.is_empty() => {
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
