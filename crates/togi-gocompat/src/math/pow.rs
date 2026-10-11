//! Go's `math.Pow` (`pow.go`), statement for statement, with the `Frexp`, `Ldexp` and
//! `Modf` it calls.
//!
//! Go runs this pure-Go `pow` on amd64 and arm64: only s390x has an assembly `archPow`,
//! and `Frexp` and `Ldexp` have no assembly outside s390x either. `pow` has no product
//! feeding an addition, so the arm64 backend fuses nothing in it, and the targets differ
//! only in the `Exp` and `Log` it calls for a fractional exponent, which the caller
//! passes in. `Modf` truncates and `Sqrt` is correctly rounded on every target.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    reason = "the comparisons and conversions are Go's own, copied verbatim"
)]

#[cfg(test)]
mod tests;

const NAN_BITS: u64 = 0x7ff8_0000_0000_0001;
const SHIFT: u32 = 52;
const MASK: u64 = 0x7ff;
const BIAS: i64 = 1023;

fn is_odd_int(x: f64) -> bool {
    if x.abs() >= (1u64 << 53) as f64 {
        return false;
    }
    let (xi, xf) = modf(x);
    xf == 0.0 && (xi as i64) & 1 == 1
}

fn modf(f: f64) -> (f64, f64) {
    let integer = f.trunc();
    (integer, (f - integer).copysign(f))
}

fn normalize(x: f64) -> (f64, i64) {
    if x.abs() < f64::MIN_POSITIVE {
        return (x * (1u64 << 52) as f64, -52);
    }
    (x, 0)
}

fn frexp(f: f64) -> (f64, i64) {
    if f == 0.0 || f.is_infinite() || f.is_nan() {
        return (f, 0);
    }
    let (f, mut exp) = normalize(f);
    let mut x = f.to_bits();
    exp += ((x >> SHIFT) & MASK) as i64 - BIAS + 1;
    x &= !(MASK << SHIFT);
    x |= ((-1 + BIAS) as u64) << SHIFT;
    (f64::from_bits(x), exp)
}

fn ldexp(frac: f64, mut exp: i64) -> f64 {
    if frac == 0.0 || frac.is_infinite() || frac.is_nan() {
        return frac;
    }
    let (frac, e) = normalize(frac);
    exp += e;
    let mut x = frac.to_bits();
    exp += ((x >> SHIFT) & MASK) as i64 - BIAS;
    if exp < -1075 {
        return 0.0_f64.copysign(frac);
    }
    if exp > 1023 {
        if frac < 0.0 {
            return f64::NEG_INFINITY;
        }
        return f64::INFINITY;
    }
    let mut m = 1.0;
    if exp < -1022 {
        exp += 53;
        m = 1.0 / (1u64 << 53) as f64;
    }
    x &= !(MASK << SHIFT);
    x |= ((exp + BIAS) as u64) << SHIFT;
    m * f64::from_bits(x)
}

/// `x` to the power `y` as Go's `pow` computes it, with `exp` and `log` the target's
/// `math.Exp` and `math.Log`.
pub(super) fn pow(x: f64, y: f64, exp: fn(f64) -> f64, log: fn(f64) -> f64) -> f64 {
    if y == 0.0 || x == 1.0 {
        return 1.0;
    } else if y == 1.0 {
        return x;
    } else if x.is_nan() || y.is_nan() {
        return f64::from_bits(NAN_BITS);
    } else if x == 0.0 {
        if y < 0.0 {
            if x.is_sign_negative() && is_odd_int(y) {
                return f64::NEG_INFINITY;
            }
            return f64::INFINITY;
        } else if y > 0.0 {
            if x.is_sign_negative() && is_odd_int(y) {
                return x;
            }
            return 0.0;
        }
    } else if y.is_infinite() {
        if x == -1.0 {
            return 1.0;
        } else if (x.abs() < 1.0) == (y == f64::INFINITY) {
            return 0.0;
        }
        return f64::INFINITY;
    } else if x.is_infinite() {
        if x == f64::NEG_INFINITY {
            return pow(1.0 / x, -y, exp, log);
        }
        if y < 0.0 {
            return 0.0;
        } else if y > 0.0 {
            return f64::INFINITY;
        }
    } else if y == 0.5 {
        return x.sqrt();
    } else if y == -0.5 {
        return 1.0 / x.sqrt();
    }

    let (mut yi, mut yf) = modf(y.abs());
    if yf != 0.0 && x < 0.0 {
        return f64::from_bits(NAN_BITS);
    }
    if yi >= 9_223_372_036_854_775_808.0 {
        if x == -1.0 {
            return 1.0;
        } else if (x.abs() < 1.0) == (y > 0.0) {
            return 0.0;
        }
        return f64::INFINITY;
    }

    let mut a1 = 1.0;
    let mut ae: i64 = 0;

    if yf != 0.0 {
        if yf > 0.5 {
            yf -= 1.0;
            yi += 1.0;
        }
        a1 = exp(yf * log(x));
    }

    let (mut x1, mut xe) = frexp(x);
    let mut i = yi as i64;
    while i != 0 {
        if !(-(1 << 12)..=1 << 12).contains(&xe) {
            ae += xe;
            break;
        }
        if i & 1 == 1 {
            a1 *= x1;
            ae += xe;
        }
        x1 *= x1;
        xe <<= 1;
        if x1 < 0.5 {
            x1 += x1;
            xe -= 1;
        }
        i >>= 1;
    }

    if y < 0.0 {
        a1 = 1.0 / a1;
        ae = -ae;
    }
    ldexp(a1, ae)
}
