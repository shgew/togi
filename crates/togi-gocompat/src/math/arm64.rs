//! Go's arm64 assembly `archExp` (`exp_arm64.s`).

#![expect(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::approx_constant,
    clippy::many_single_char_names,
    reason = "the constants and conversions are Go's own, copied verbatim"
)]

use super::fma;

#[inline(always)]
pub(super) fn exp(x: f64) -> f64 {
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const LOG2E: f64 = 1.44269504088896338700e+00;
    const OVERFLOW: f64 = 7.09782712893383973096e+02;
    const UNDERFLOW: f64 = -7.45133219101941108420e+02;
    const NEAR_ZERO: f64 = 1.0 / (1u64 << 28) as f64;
    const P1: f64 = 1.66666666666666657415e-01;
    const P2: f64 = -2.77777777770155933842e-03;
    const P3: f64 = 6.61375632143793436117e-05;
    const P4: f64 = -1.65339022054652515390e-06;
    const P5: f64 = 4.13813679705723846039e-08;

    if x.is_nan() {
        return x;
    }
    if x > OVERFLOW {
        return f64::INFINITY;
    }
    if x < UNDERFLOW {
        return 0.0;
    }
    if x.abs() < NEAR_ZERO {
        return 1.0 + x;
    }

    let k = if x < 0.0 {
        fma(LOG2E, x, -0.5)
    } else {
        fma(LOG2E, x, 0.5)
    } as i64;
    let kf = k as f64;
    let hi = fma(-kf, LN2_HI, x);
    let lo = kf * LN2_LO;
    let r = hi - lo;
    let t = r * r;

    let mut c = fma(t, P5, P4);
    c = fma(t, c, P3);
    c = fma(t, c, P2);
    c = fma(t, c, P1);
    c = fma(-t, c, r);
    let y = 1.0 - ((lo - (r * c) / (2.0 - c)) - hi);

    let bits = y.to_bits();
    let frac = bits & 0x000f_ffff_ffff_ffff;
    let mut exponent = (bits >> 52) as i64 + k;
    let mut m = 1.0;
    if exponent < 1 {
        exponent += 52;
        m = f64::from_bits(0x3cb0_0000_0000_0000);
    }
    m * f64::from_bits(((exponent as u64) << 52) | frac)
}
