//! Independent random streams keyed by typed domains.
//!
//! A stream is a [`StdRng`] seeded from a splitmix64 fold over the run seed, the domain and the key parts.
//! The fold is integer arithmetic fixed here, so it cannot move with a Rust release; the generator behind
//! [`StdRng`] is pinned by `Cargo.lock`. Boot identity and synthetic telemetry do not share state.

use rand::SeedableRng;
use rand::rngs::StdRng;
use togi_machine::Regime;

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
#[derive(Clone, Copy, Debug)]
pub(crate) enum Domain {
    /// The boot ID of a numbered boot.
    Boot,
    /// Synthetic maximum temperature for a trial.
    Tctl,
}

impl Domain {
    const fn parts(self) -> [u64; 2] {
        match self {
            Self::Boot => [2, 0],
            Self::Tctl => [11, 0],
        }
    }
}

/// The stream of `domain` for `seed` and `key`. The same arguments always give the same stream.
pub(crate) fn stream(seed: u64, domain: Domain, key: &[u64]) -> StdRng {
    let mut state = absorb(GOLDEN, seed);
    for part in domain.parts() {
        state = absorb(state, part);
    }
    for &part in key {
        state = absorb(state, part);
    }
    state = absorb(state, key.len() as u64);
    let mut bytes = [0u8; 32];
    for chunk in bytes.as_chunks_mut::<8>().0 {
        state = splitmix64(state);
        *chunk = state.to_le_bytes();
    }
    StdRng::from_seed(bytes)
}

/// A regime's number in the synthetic Tctl stream. These key parts must never be renumbered.
pub(crate) const fn regime_key(regime: Regime) -> u64 {
    match regime {
        Regime::R1 => 0,
        Regime::R2 => 1,
        Regime::R3 => 2,
        Regime::R4 => 3,
        Regime::R5 => 4,
        Regime::R6 => 5,
        Regime::R7 => 6,
        Regime::R8 => 7,
        Regime::R9 => 8,
    }
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
        let mut a = stream(7, Domain::Tctl, &[1, 2, 3]);
        let mut b = stream(7, Domain::Tctl, &[1, 2, 3]);
        let first: [u64; 16] = std::array::from_fn(|_| a.next_u64());
        let second: [u64; 16] = std::array::from_fn(|_| b.next_u64());
        assert_eq!(first, second);
    }

    #[test]
    fn the_fold_is_fixed_integer_arithmetic() {
        assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
        assert_eq!(absorb(0, 0), splitmix64(0));
        assert_eq!(Domain::Boot.parts(), [2, 0]);
        assert_eq!(Domain::Tctl.parts(), [11, 0]);
    }

    #[test]
    fn domains_seeds_and_keys_give_independent_streams() {
        assert_ne!(first(Domain::Boot, &[1]), first(Domain::Tctl, &[1]));
        assert_ne!(first(Domain::Tctl, &[1]), first(Domain::Tctl, &[2]));
        assert_ne!(first(Domain::Tctl, &[1, 2]), first(Domain::Tctl, &[2, 1]));
        assert_ne!(first(Domain::Tctl, &[1]), first(Domain::Tctl, &[1, 0]));
        assert_ne!(
            stream(1, Domain::Boot, &[]).next_u64(),
            stream(2, Domain::Boot, &[]).next_u64()
        );
    }
}
