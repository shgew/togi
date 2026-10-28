use togi_machine::{CoreSet, PerCore};

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
/// The night layout derives each CCD's chain from the measured requests of the latest full-CCD R7
/// load (`policy/night_query.rs`).
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
                    remaining = remaining
                        .iter()
                        .filter(|&core| !group.contains(core))
                        .collect();
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
