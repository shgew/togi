//! Independent random streams keyed by typed domains.
//!
//! A stream is a [`StdRng`] seeded from a splitmix64 fold over the run seed, the domain and the key parts.
//! The fold is integer arithmetic fixed here, so it cannot move with a Rust release; the generator behind
//! [`StdRng`] is pinned by `Cargo.lock`. Domains never share state, so drawing from one (telemetry, say)
//! cannot change another (a failure time).

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::model::Limits;

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

const fn splitmix64(value: u64) -> u64 {
    let mut z = value.wrapping_add(GOLDEN);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

const fn absorb(state: u64, part: u64) -> u64 {
    splitmix64(state ^ part)
}

/// What a stream draws for. Tags are part of the stream and must never be renumbered.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Domain {
    /// Hidden limits of a machine without a `core` table.
    Limits,
    /// The boot ID of a numbered boot.
    Boot,
    /// A loaded core's failure time.
    Core,
    /// An unloaded core's failure time.
    Idle,
    /// A shared-rail core's failure time.
    Voltage,
    Ccd(u8),
    Joint(u32),
    Background,
    /// The signal, reset and MCE bank draws that follow a failure.
    Signal,
    /// A shared-rail core's computation-error-or-crash draw.
    VoltageSignal,
    Tctl,
    /// The choice among matching replay facts.
    Replay,
}

impl Domain {
    fn parts(self) -> [u64; 2] {
        match self {
            Self::Limits => [1, 0],
            Self::Boot => [2, 0],
            Self::Core => [3, 0],
            Self::Idle => [4, 0],
            Self::Voltage => [5, 0],
            Self::Ccd(ccd) => [6, u64::from(ccd)],
            Self::Joint(index) => [7, u64::from(index)],
            Self::Background => [8, 0],
            Self::Signal => [9, 0],
            Self::VoltageSignal => [10, 0],
            Self::Tctl => [11, 0],
            Self::Replay => [12, 0],
        }
    }
}

/// The stream of `domain` for `seed` and `key`. The same arguments always give the same stream.
pub fn stream(seed: u64, domain: Domain, key: &[u64]) -> StdRng {
    let mut state = absorb(GOLDEN, seed);
    for part in domain.parts() {
        state = absorb(state, part);
    }
    for &part in key {
        state = absorb(state, part);
    }
    state = absorb(state, u64::try_from(key.len()).unwrap_or(u64::MAX));
    let mut bytes = [0u8; 32];
    for chunk in bytes.as_chunks_mut::<8>().0 {
        state = splitmix64(state);
        *chunk = state.to_le_bytes();
    }
    StdRng::from_seed(bytes)
}

/// A key part for text, such as a workload ID.
pub fn text_part(text: &str) -> u64 {
    text.bytes()
        .fold(GOLDEN, |state, byte| absorb(state, u64::from(byte)))
}

const MIN_BASE: i64 = -40;
const MAX_BASE: i64 = -5;

/// Hidden limits drawn from `seed`: a per-core base in -40..=-5, each alone limit 0 to 2 counts
/// shallower than it, and each together limit 0 (with probability 3/4) or 1 to 3 counts shallower than
/// its alone limit. R6 and R7 start from the shallowest alone limit of the core. Everything is clamped
/// to -50..=0.
pub(crate) fn draw_limits(seed: u64, cores: usize) -> Vec<Limits> {
    let mut rng = stream(seed, Domain::Limits, &[]);
    let clamp = |offset: i64| offset.clamp(-50, 0);
    let together = |rng: &mut StdRng| {
        if rng.random_bool(0.75) {
            0
        } else {
            rng.random_range(1..=3)
        }
    };
    (0..cores)
        .map(|_| {
            let base = rng.random_range(MIN_BASE..=MAX_BASE);
            let mut alone = [0; 5];
            for limit in &mut alone {
                *limit = clamp(base + rng.random_range(0..=2));
            }
            let shallowest = alone.iter().copied().max().unwrap_or(-50);
            let mut limits = [0; 7];
            for (limit, &floor) in limits.iter_mut().zip(alone.iter().chain([&shallowest; 2])) {
                *limit = clamp(floor + together(&mut rng));
            }
            Limits {
                alone,
                together: limits,
                idle: None,
                flat: 0.0,
                workload: Vec::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    fn first(domain: Domain, key: &[u64]) -> u64 {
        stream(7, domain, key).next_u64()
    }

    #[test]
    fn a_stream_repeats_for_the_same_arguments() {
        let mut a = stream(7, Domain::Core, &[1, 2, 3]);
        let mut b = stream(7, Domain::Core, &[1, 2, 3]);
        let first: [u64; 16] = std::array::from_fn(|_| a.next_u64());
        let second: [u64; 16] = std::array::from_fn(|_| b.next_u64());
        assert_eq!(first, second);
    }

    #[test]
    fn the_fold_is_fixed_integer_arithmetic() {
        assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
        assert_eq!(absorb(0, 0), splitmix64(0));
        assert_eq!(
            stream(0, Domain::Limits, &[]).next_u64(),
            stream(0, Domain::Limits, &[]).next_u64()
        );
    }

    #[test]
    fn domains_seeds_and_keys_give_independent_streams() {
        let domains = [
            Domain::Limits,
            Domain::Boot,
            Domain::Core,
            Domain::Idle,
            Domain::Voltage,
            Domain::Ccd(0),
            Domain::Ccd(1),
            Domain::Joint(0),
            Domain::Joint(1),
            Domain::Background,
            Domain::Signal,
            Domain::VoltageSignal,
            Domain::Tctl,
            Domain::Replay,
        ];
        let mut seen = std::collections::HashSet::new();
        for domain in domains {
            assert!(seen.insert(first(domain, &[1])), "{domain:?}");
        }
        assert_ne!(first(Domain::Core, &[1]), first(Domain::Core, &[2]));
        assert_ne!(first(Domain::Core, &[1, 2]), first(Domain::Core, &[2, 1]));
        assert_ne!(first(Domain::Core, &[1]), first(Domain::Core, &[1, 0]));
        assert_ne!(
            stream(1, Domain::Core, &[]).next_u64(),
            stream(2, Domain::Core, &[]).next_u64()
        );
    }
}
