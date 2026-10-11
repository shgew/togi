//! Go's `math.Exp`, `math.Expm1`, `math.Log` and `math.Log1p`, bit for bit.
//!
//! Go does not run one algorithm for each function. It runs assembly for some and
//! pure Go for the rest, and the Go compiler fuses multiplications into additions on
//! arm64 but not on amd64, so the results differ between targets in the last bit.
//! Each function here follows the path Go runs on the target it is built for:
//!
//! | function | amd64 | arm64 |
//! | --- | --- | --- |
//! | `exp` | `archExp`, assembly with an FMA branch taken when AVX and FMA are present | `archExp`, assembly with fused multiply-adds |
//! | `expm1` | pure Go, no fusion | pure Go, fused as the arm64 compiler fuses |
//! | `log` | `archLog`, assembly | pure Go, fused |
//! | `log1p` | pure Go, no fusion | pure Go, fused |
//!
//! The platform libm is never called: fused operations go through `f64::mul_add`
//! inside code compiled for the hardware instruction.

/// Fused multiply-add, `a * b + c` rounded once, as the hardware instruction.
///
/// On `x86_64` this is the FMA instruction, never the platform libm, so it must only run
/// in code compiled with the `fma` target feature: `amd64::exp_fma` and the test wrappers
/// that run the arm64 port. Everything that reaches it with fusion on is `inline(always)`.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
#[expect(unsafe_code, reason = "the FMA intrinsic needs the target feature")]
fn fma(a: f64, b: f64, c: f64) -> f64 {
    use std::arch::x86_64::{_mm_cvtsd_f64, _mm_fmadd_sd, _mm_set_sd};
    // SAFETY: every caller is inlined into a function compiled with the fma target feature.
    unsafe { _mm_cvtsd_f64(_mm_fmadd_sd(_mm_set_sd(a), _mm_set_sd(b), _mm_set_sd(c))) }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "keeps the helper out of call overhead like the instruction it names"
)]
fn fma(a: f64, b: f64, c: f64) -> f64 {
    a.mul_add(b, c)
}

#[cfg(target_arch = "x86_64")]
mod amd64;
#[cfg(any(target_arch = "aarch64", all(test, target_arch = "x86_64")))]
mod arm64;
mod go;
#[cfg(test)]
mod tests;

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("togi-gocompat reproduces Go's math on x86_64 and aarch64 only");

#[cfg(target_arch = "x86_64")]
#[must_use]
pub fn exp(x: f64) -> f64 {
    amd64::exp(x)
}

#[cfg(target_arch = "x86_64")]
#[must_use]
pub fn expm1(x: f64) -> f64 {
    go::expm1::<false>(x)
}

#[cfg(target_arch = "x86_64")]
#[must_use]
pub fn log(x: f64) -> f64 {
    amd64::log(x)
}

#[cfg(target_arch = "x86_64")]
#[must_use]
pub fn log1p(x: f64) -> f64 {
    go::log1p::<false>(x)
}

#[cfg(target_arch = "aarch64")]
#[must_use]
pub fn exp(x: f64) -> f64 {
    arm64::exp(x)
}

#[cfg(target_arch = "aarch64")]
#[must_use]
pub fn expm1(x: f64) -> f64 {
    go::expm1::<true>(x)
}

#[cfg(target_arch = "aarch64")]
#[must_use]
pub fn log(x: f64) -> f64 {
    go::log::<true>(x)
}

#[cfg(target_arch = "aarch64")]
#[must_use]
pub fn log1p(x: f64) -> f64 {
    go::log1p::<true>(x)
}
