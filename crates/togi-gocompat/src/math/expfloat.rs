//! Go's `math/rand/v2` `ExpFloat64`, the ziggurat of Marsaglia and Tsang, draw for draw.
//!
//! The algorithm takes its draws from a [`Uint64Source`], which the PCG implements when
//! it lands. Go's `Rand.Float64` keeps the low 53 bits of a draw and divides by 2^53.
//! The tail calls `Log` and the rejection test calls `Exp`, each the target's own port.
//! On arm64 Go also fuses the float32 multiplication and addition of the rejection test
//! into one `FMADDS`, so `FUSED` rounds them once there and twice on amd64.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::many_single_char_names,
    reason = "the conversions are Go's own, copied verbatim"
)]

mod tables;
#[cfg(test)]
mod tests;

use tables::{FE, KE, WE};

const RE: f64 = 7.69711747013104972;

/// A stream of the 64-bit draws Go's `rand.Source` returns from `Uint64`.
pub trait Uint64Source {
    fn uint64(&mut self) -> u64;
}

fn float64(src: &mut impl Uint64Source) -> f64 {
    ((src.uint64() << 11 >> 11) as f64) / (1u64 << 53) as f64
}

/// One call of Go's `ExpFloat64` on `src`, with `exp` and `log` the target's `math.Exp`
/// and `math.Log`.
pub(super) fn exp_float64<const FUSED: bool>(
    src: &mut impl Uint64Source,
    exp: impl Fn(f64) -> f64,
    log: impl Fn(f64) -> f64,
) -> f64 {
    loop {
        let u = src.uint64();
        let j = u as u32;
        let i = usize::from((u >> 32) as u8);
        let x = f64::from(j) * f64::from(f32::from_bits(WE[i]));
        if j < KE[i] {
            return x;
        }
        if i == 0 {
            return RE - log(float64(src));
        }
        let fe = f32::from_bits(FE[i]);
        let step = f32::from_bits(FE[i - 1]) - fe;
        let f = float64(src) as f32;
        let bound = if FUSED {
            f.mul_add(step, fe)
        } else {
            fe + f * step
        };
        if bound < exp(-x) as f32 {
            return x;
        }
    }
}
