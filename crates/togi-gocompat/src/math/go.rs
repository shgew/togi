//! Go's pure-Go `expm1`, `log1p` and `log`, statement for statement.
//!
//! With `FUSED` the products are fused where Go's arm64 backend fuses them: every
//! `a + x*y`, `a - x*y` and `x*y - a` whose product is a multiplication node, whether
//! or not that product has other uses. The helpers below name the three forms. Without
//! it each product rounds first, which is what Go does on amd64 with `GOAMD64=v1`.

#![expect(
    clippy::excessive_precision,
    clippy::approx_constant,
    clippy::unreadable_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::many_single_char_names,
    clippy::inline_always,
    reason = "the constants and conversions are Go's own, copied verbatim"
)]

const FRAC_MASK: u64 = 0x000f_ffff_ffff_ffff;
const NAN_BITS: u64 = 0x7ff8_0000_0000_0001;
#[cfg(any(target_arch = "aarch64", test))]
const SQRT2: f64 = 1.41421356237309504880168872420969807856967187537694807317667974;

fn nan() -> f64 {
    f64::from_bits(NAN_BITS)
}

#[inline(always)]
pub(super) fn madd<const FUSED: bool>(a: f64, x: f64, y: f64) -> f64 {
    if FUSED {
        super::fma(x, y, a)
    } else {
        a + x * y
    }
}

#[inline(always)]
pub(super) fn msub<const FUSED: bool>(a: f64, x: f64, y: f64) -> f64 {
    if FUSED {
        super::fma(-x, y, a)
    } else {
        a - x * y
    }
}

#[inline(always)]
pub(super) fn nmsub<const FUSED: bool>(a: f64, x: f64, y: f64) -> f64 {
    if FUSED {
        super::fma(x, y, -a)
    } else {
        x * y - a
    }
}

#[cfg(any(target_arch = "aarch64", test))]
#[inline(always)]
fn frexp(f: f64) -> (f64, i64) {
    let (f, mut exp) = if f.abs() < f64::MIN_POSITIVE {
        (f * (1u64 << 52) as f64, -52)
    } else {
        (f, 0)
    };
    let mut x = f.to_bits();
    exp += ((x >> 52) & 0x7ff) as i64 - 1023 + 1;
    x &= !(0x7ff << 52);
    x |= 1022 << 52;
    (f64::from_bits(x), exp)
}

#[cfg(any(target_arch = "aarch64", test))]
#[inline(always)]
pub(super) fn log<const FUSED: bool>(x: f64) -> f64 {
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const L1: f64 = 6.666666666666735130e-01;
    const L2: f64 = 3.999999999940941908e-01;
    const L3: f64 = 2.857142874366239149e-01;
    const L4: f64 = 2.222219843214978396e-01;
    const L5: f64 = 1.818357216161805012e-01;
    const L6: f64 = 1.531383769920937332e-01;
    const L7: f64 = 1.479819860511658591e-01;

    if x.is_nan() || x == f64::INFINITY {
        return x;
    }
    if x < 0.0 {
        return nan();
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }

    let (mut f1, mut ki) = frexp(x);
    if f1 < SQRT2 / 2.0 {
        f1 *= 2.0;
        ki -= 1;
    }
    let f = f1 - 1.0;
    let k = ki as f64;

    let s = f / (2.0 + f);
    let s2 = s * s;
    let s4 = s2 * s2;
    let inner1 = madd::<FUSED>(L1, s4, madd::<FUSED>(L3, s4, madd::<FUSED>(L5, s4, L7)));
    let t2 = s4 * madd::<FUSED>(L2, s4, madd::<FUSED>(L4, s4, L6));
    let r = madd::<FUSED>(t2, s2, inner1);
    let half_f = 0.5 * f;
    let hfsq_plus_r = madd::<FUSED>(r, half_f, f);
    let sum = madd::<FUSED>(k * LN2_LO, s, hfsq_plus_r);
    let diff = nmsub::<FUSED>(sum, half_f, f);
    nmsub::<FUSED>(diff - f, k, LN2_HI)
}

#[inline(always)]
pub(super) fn expm1<const FUSED: bool>(mut x: f64) -> f64 {
    const OTHRESHOLD: f64 = 7.09782712893383973096e+02;
    const LN2X56: f64 = 3.88162421113569373274e+01;
    const LN2_HALF_X3: f64 = 1.03972077083991796413e+00;
    const LN2_HALF: f64 = 3.46573590279972654709e-01;
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const INV_LN2: f64 = 1.44269504088896338700e+00;
    const TINY: f64 = 1.0 / (1u64 << 54) as f64;
    const Q1: f64 = -3.33333333333331316428e-02;
    const Q2: f64 = 1.58730158725481460165e-03;
    const Q3: f64 = -7.93650757867487942473e-05;
    const Q4: f64 = 4.00821782732936239552e-06;
    const Q5: f64 = -2.01099218183624371326e-07;

    if x == f64::INFINITY || x.is_nan() {
        return x;
    }
    if x == f64::NEG_INFINITY {
        return -1.0;
    }

    let mut absx = x;
    let mut sign = false;
    if x < 0.0 {
        absx = -absx;
        sign = true;
    }

    if absx >= LN2X56 {
        if sign {
            return -1.0;
        }
        if absx >= OTHRESHOLD {
            return f64::INFINITY;
        }
    }

    let mut c = 0.0;
    let k: i64;
    if absx > LN2_HALF {
        let hi;
        let lo;
        if absx < LN2_HALF_X3 {
            if sign {
                hi = x + LN2_HI;
                lo = -LN2_LO;
                k = -1;
            } else {
                hi = x - LN2_HI;
                lo = LN2_LO;
                k = 1;
            }
        } else {
            k = if sign {
                nmsub::<FUSED>(0.5, INV_LN2, x) as i64
            } else {
                madd::<FUSED>(0.5, INV_LN2, x) as i64
            };
            let t = k as f64;
            hi = msub::<FUSED>(x, t, LN2_HI);
            lo = t * LN2_LO;
        }
        x = hi - lo;
        c = (hi - x) - lo;
    } else if absx < TINY {
        return x;
    } else {
        k = 0;
    }

    let hfx = 0.5 * x;
    let hxs = x * hfx;
    let r1 = madd::<FUSED>(
        1.0,
        hxs,
        madd::<FUSED>(
            Q1,
            hxs,
            madd::<FUSED>(Q2, hxs, madd::<FUSED>(Q3, hxs, madd::<FUSED>(Q4, hxs, Q5))),
        ),
    );
    let t = msub::<FUSED>(3.0, r1, hfx);
    let quotient = (r1 - t) / msub::<FUSED>(6.0, x, t);
    if k == 0 {
        let e = hxs * quotient;
        return x - msub::<FUSED>(x * e, x, hfx);
    }
    let e_minus_c = nmsub::<FUSED>(c, hxs, quotient);
    let e = nmsub::<FUSED>(c, x, e_minus_c);
    let e = msub::<FUSED>(e, x, hfx);
    let add_k = |y: f64| f64::from_bits(y.to_bits().wrapping_add((k as u64) << 52));
    if k == -1 {
        return nmsub::<FUSED>(0.5, 0.5, x - e);
    }
    if k == 1 {
        if x < -0.25 {
            return -2.0 * (e - (x + 0.5));
        }
        return 1.0 + 2.0 * (x - e);
    }
    if k <= -2 || k > 56 {
        return add_k(1.0 - (e - x)) - 1.0;
    }
    if k < 20 {
        let t = f64::from_bits(0x3ff0_0000_0000_0000 - (0x0020_0000_0000_0000 >> k));
        return add_k(t - (e - x));
    }
    let t = f64::from_bits((0x3ff - k as u64) << 52);
    let mut y = x - (e + t);
    y += 1.0;
    add_k(y)
}

#[inline(always)]
pub(super) fn log1p<const FUSED: bool>(x: f64) -> f64 {
    const SQRT2_M1: f64 = 4.142135623730950488017e-01;
    const SQRT2_HALF_M1: f64 = -2.928932188134524755992e-01;
    const SMALL: f64 = 1.0 / (1u64 << 29) as f64;
    const TINY: f64 = 1.0 / (1u64 << 54) as f64;
    const TWO53: f64 = (1u64 << 53) as f64;
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const LP1: f64 = 6.666666666666735130e-01;
    const LP2: f64 = 3.999999999940941908e-01;
    const LP3: f64 = 2.857142874366239149e-01;
    const LP4: f64 = 2.222219843214978396e-01;
    const LP5: f64 = 1.818357216161805012e-01;
    const LP6: f64 = 1.531383769920937332e-01;
    const LP7: f64 = 1.479819860511658591e-01;

    if x < -1.0 || x.is_nan() {
        return nan();
    }
    if x == -1.0 {
        return f64::NEG_INFINITY;
    }
    if x == f64::INFINITY {
        return f64::INFINITY;
    }

    let absx = x.abs();

    let mut f = 0.0;
    let mut iu = 0u64;
    let mut k = 1i64;
    if absx < SQRT2_M1 {
        if absx < SMALL {
            if absx < TINY {
                return x;
            }
            return msub::<FUSED>(x, x * x, 0.5);
        }
        if x > SQRT2_HALF_M1 {
            k = 0;
            f = x;
            iu = 1;
        }
    }
    let mut c = 0.0;
    if k != 0 {
        let mut u;
        if absx < TWO53 {
            u = 1.0 + x;
            iu = u.to_bits();
            k = (iu >> 52).wrapping_sub(1023) as i64;
            if k > 0 {
                c = 1.0 - (u - x);
            } else {
                c = x - (u - 1.0);
            }
            c /= u;
        } else {
            u = x;
            iu = u.to_bits();
            k = (iu >> 52).wrapping_sub(1023) as i64;
            c = 0.0;
        }
        iu &= FRAC_MASK;
        if iu < 0x0006_a09e_667f_3bcd {
            u = f64::from_bits(iu | 0x3ff0_0000_0000_0000);
        } else {
            k += 1;
            u = f64::from_bits(iu | 0x3fe0_0000_0000_0000);
            iu = (0x0010_0000_0000_0000 - iu) >> 2;
        }
        f = u - 1.0;
    }
    let half_f = 0.5 * f;
    let hfsq = half_f * f;
    let kf = k as f64;
    if iu == 0 {
        if f == 0.0 {
            if k == 0 {
                return 0.0;
            }
            c = madd::<FUSED>(c, kf, LN2_LO);
            return madd::<FUSED>(c, kf, LN2_HI);
        }
        let factor = msub::<FUSED>(1.0, 0.66666666666666666, f);
        if k == 0 {
            return msub::<FUSED>(f, hfsq, factor);
        }
        let a = madd::<FUSED>(c, kf, LN2_LO);
        let b = nmsub::<FUSED>(a, hfsq, factor);
        return nmsub::<FUSED>(b - f, kf, LN2_HI);
    }
    let s = f / (2.0 + f);
    let z = s * s;
    let poly = madd::<FUSED>(
        LP1,
        z,
        madd::<FUSED>(
            LP2,
            z,
            madd::<FUSED>(
                LP3,
                z,
                madd::<FUSED>(LP4, z, madd::<FUSED>(LP5, z, madd::<FUSED>(LP6, z, LP7))),
            ),
        ),
    );
    let hfsq_plus_r = madd::<FUSED>(z * poly, half_f, f);
    if k == 0 {
        return f - msub::<FUSED>(hfsq, s, hfsq_plus_r);
    }
    let a = madd::<FUSED>(c, kf, LN2_LO);
    let e = madd::<FUSED>(a, s, hfsq_plus_r);
    let g = nmsub::<FUSED>(e, half_f, f);
    nmsub::<FUSED>(g - f, kf, LN2_HI)
}
