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
//! No transcendental libm function is called. Fused operations go through
//! `f64::mul_add`, which is correctly rounded by contract, so its bits do not depend
//! on whether it becomes the FMA instruction or a call to libm's `fma`.

/// Fused multiply-add, `a * b + c` rounded once, as Go's fused instructions round it.
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
