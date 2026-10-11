//! Go's amd64 assembly `archExp` (`exp_amd64.s`) and `archLog` (`log_amd64.s`).

#![expect(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::approx_constant,
    clippy::many_single_char_names,
    reason = "the constants and conversions are Go's own, copied verbatim"
)]

use super::fma;

const SIGN_MASK: u64 = !(1 << 63);
const POS_INF: u64 = 0x7ff0_0000_0000_0000;
const NEG_INF: u64 = 0xfff0_0000_0000_0000;
const NAN_BITS: u64 = 0x7ff8_0000_0000_0001;

pub(super) fn use_fma() -> bool {
    is_x86_feature_detected!("avx") && is_x86_feature_detected!("fma")
}

pub(super) fn exp(x: f64) -> f64 {
    if use_fma() {
        exp_fma_checked(x)
    } else {
        exp_plain(x)
    }
}

#[expect(unsafe_code, reason = "calls the FMA variant after the run-time check")]
fn exp_fma_checked(x: f64) -> f64 {
    // SAFETY: the caller has checked that the CPU supports FMA.
    unsafe { exp_fma(x) }
}

#[target_feature(enable = "fma")]
pub(super) fn exp_fma(x: f64) -> f64 {
    exp_impl::<true>(x)
}

pub(super) fn exp_plain(x: f64) -> f64 {
    exp_impl::<false>(x)
}

fn cvtsd2sl(v: f64) -> i32 {
    let r = v.round_ties_even();
    if (-2_147_483_648.0..=2_147_483_647.0).contains(&r) {
        r as i32
    } else {
        i32::MIN
    }
}

#[inline(always)]
fn exp_impl<const FMA: bool>(x: f64) -> f64 {
    const LOG2E: f64 = 1.4426950408889634073599246810018920;
    const LN2U: f64 = 0.69314718055966295651160180568695068359375;
    const LN2L: f64 = 0.28235290563031577122588448175013436025525412068e-12;
    const OVERFLOW: f64 = 7.09782712893384e+02;
    const C0: f64 = 0.5;
    const C8: f64 = 1.0;
    const C16: f64 = 2.0;
    const C24: f64 = 1.6666666666666666667e-1;
    const C32: f64 = 4.1666666666666666667e-2;
    const C40: f64 = 8.3333333333333333333e-3;
    const C48: f64 = 1.3888888888888888889e-3;
    const C56: f64 = 1.9841269841269841270e-4;
    const C64: f64 = 2.4801587301587301587e-5;

    let bits = x.to_bits();
    if bits & SIGN_MASK >= POS_INF {
        return if bits == NEG_INF { 0.0 } else { x };
    }
    if x > OVERFLOW {
        return f64::INFINITY;
    }
    let k = cvtsd2sl(LOG2E * x);
    let kf = f64::from(k);

    let mut r;
    let mut p;
    if FMA {
        r = fma(-kf, LN2U, x);
        r = fma(-kf, LN2L, r);
        r *= 0.0625;
        p = C64;
        for c in [C56, C48, C40, C32, C24, C0, C8] {
            p = fma(r, p, c);
        }
        r *= p;
        for _ in 0..3 {
            let t = r + C16;
            r *= t;
        }
        let t = r + C16;
        r = fma(t, r, C8);
    } else {
        r = x - LN2U * kf;
        r -= LN2L * kf;
        r *= 0.0625;
        p = C64 * r;
        for c in [C56, C48, C40, C32, C24, C0] {
            p += c;
            p *= r;
        }
        p += C8;
        r *= p;
        for _ in 0..4 {
            let t = C16 + r;
            r *= t;
        }
        r += C8;
    }

    let mut bx = k.wrapping_add(0x3ff);
    if bx <= 0 {
        if bx < -52 {
            return 0.0;
        }
        bx += 0x3fe;
        r *= f64::from_bits(u64::from(bx as u32) << 52);
        bx = 1;
    } else if bx >= 0x7ff {
        return f64::INFINITY;
    }
    r * f64::from_bits(u64::from(bx as u32) << 52)
}

pub(super) fn log(x: f64) -> f64 {
    const HSQRT2: f64 = 7.07106781186547524401e-01;
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const L1: f64 = 6.666666666666735130e-01;
    const L2: f64 = 3.999999999940941908e-01;
    const L3: f64 = 2.857142874366239149e-01;
    const L4: f64 = 2.222219843214978396e-01;
    const L5: f64 = 1.818357216161805012e-01;
    const L6: f64 = 1.531383769920937332e-01;
    const L7: f64 = 1.479819860511658591e-01;

    let bits = x.to_bits();
    if bits & SIGN_MASK == 0 {
        return f64::NEG_INFINITY;
    }
    if (bits as i64) < 0 {
        return f64::from_bits(NAN_BITS);
    }
    if bits >= POS_INF {
        return x;
    }

    let f1 = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0.5f64.to_bits());
    let mut k = f64::from(((bits >> 52) & 0x7ff) as i32 - 0x3fe);
    let mask = if f1 <= HSQRT2 { 1.0 } else { 0.0 };
    k -= mask;
    let f = f1 * (mask + 1.0) - 1.0;

    let s = f / (2.0 + f);
    let s2 = s * s;
    let s4 = s2 * s2;
    let mut t1 = L7 * s4;
    t1 += L5;
    t1 *= s4;
    t1 += L3;
    t1 *= s4;
    t1 += L1;
    let t1 = s2 * t1;
    let mut t2 = L6 * s4;
    t2 += L4;
    t2 *= s4;
    t2 += L2;
    let t2 = s4 * t2;
    let r = t1 + t2;
    let hfsq = 0.5 * f * f;
    let hfsq_plus_r = r + hfsq;
    let inner = s * hfsq_plus_r + LN2_LO * k;
    let outer = hfsq - inner - f;
    k * LN2_HI - outer
}
