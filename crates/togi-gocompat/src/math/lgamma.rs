//! Go's pure-Go `Lgamma` (`lgamma.go`) with its `sinPi`, and the pure-Go `sin` and
//! `cos` (`sin.go`) it calls, statement for statement.
//!
//! `FUSED` fuses products as Go's arm64 backend does, read from the disassembly:
//! every `a + x*y`, `a - x*y` and `x*y - a`, and in `y*p1 + p2` the right-hand product
//! `p2`. `log` is the `Log` Go runs on the same target. Go's `Floor` and `Mod` are
//! exact on every target, so `f64::floor` and `%` return the same bits for the
//! positive finite arguments `sin_pi` passes them.

#![expect(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::float_cmp,
    clippy::many_single_char_names,
    clippy::inline_always,
    reason = "the constants and conversions are Go's own, copied verbatim"
)]

use super::go::{madd, msub, nmsub};
use std::f64::consts::PI;

const TWO52: f64 = (1u64 << 52) as f64;
const TWO53: f64 = (1u64 << 53) as f64;
const TWO58: f64 = (1u64 << 58) as f64;

const LGAM_A: [f64; 12] = [
    7.72156649015328655494e-02,
    3.22467033424113591611e-01,
    6.73523010531292681824e-02,
    2.05808084325167332806e-02,
    7.38555086081402883957e-03,
    2.89051383673415629091e-03,
    1.19270763183362067845e-03,
    5.10069792153511336608e-04,
    2.20862790713908385557e-04,
    1.08011567247583939954e-04,
    2.52144565451257326939e-05,
    4.48640949618915160150e-05,
];
const LGAM_R: [f64; 7] = [
    1.0,
    1.39200533467621045958e+00,
    7.21935547567138069525e-01,
    1.71933865632803078993e-01,
    1.86459191715652901344e-02,
    7.77942496381893596434e-04,
    7.32668430744625636189e-06,
];
const LGAM_S: [f64; 7] = [
    -7.72156649015328655494e-02,
    2.14982415960608852501e-01,
    3.25778796408930981787e-01,
    1.46350472652464452805e-01,
    2.66422703033638609560e-02,
    1.84028451407337715652e-03,
    3.19475326584100867617e-05,
];
const LGAM_T: [f64; 15] = [
    4.83836122723810047042e-01,
    -1.47587722994593911752e-01,
    6.46249402391333854778e-02,
    -3.27885410759859649565e-02,
    1.79706750811820387126e-02,
    -1.03142241298341437450e-02,
    6.10053870246291332635e-03,
    -3.68452016781138256760e-03,
    2.25964780900612472250e-03,
    -1.40346469989232843813e-03,
    8.81081882437654011382e-04,
    -5.38595305356740546715e-04,
    3.15632070903625950361e-04,
    -3.12754168375120860518e-04,
    3.35529192635519073543e-04,
];
const LGAM_U: [f64; 6] = [
    -7.72156649015328655494e-02,
    6.32827064025093366517e-01,
    1.45492250137234768737e+00,
    9.77717527963372745603e-01,
    2.28963728064692451092e-01,
    1.33810918536787660377e-02,
];
const LGAM_V: [f64; 6] = [
    1.0,
    2.45597793713041134822e+00,
    2.12848976379893395361e+00,
    7.69285150456672783825e-01,
    1.04222645593369134254e-01,
    3.21709242282423911810e-03,
];
const LGAM_W: [f64; 7] = [
    4.18938533204672725052e-01,
    8.33333333333329678849e-02,
    -2.77777777728775536470e-03,
    7.93650558643019558500e-04,
    -5.95187557450339963135e-04,
    8.36339918996282139126e-04,
    -1.63092934096575273989e-03,
];

/// Go's `Lgamma`: the natural logarithm of `|Γ(x)|` and the sign of `Γ(x)`.
#[inline(always)]
pub(super) fn lgamma<const FUSED: bool>(x: f64, log: impl Fn(f64) -> f64) -> (f64, i32) {
    const TINY: f64 = 1.0 / (1u128 << 70) as f64;
    const TC: f64 = 1.46163214496836224576e+00;
    const TF: f64 = -1.21486290535849611461e-01;
    const TT: f64 = -3.63867699703950536541e-18;
    // Go folds `Ymin - 1 + 0.27` and the others exactly before rounding once.
    const YMIN_MINUS_1_PLUS_027: f64 = 0.7316321449683623;
    const YMIN_MINUS_1_MINUS_027: f64 = 0.19163214496836226;
    const YMIN_PLUS_027: f64 = 1.7316321449683623;
    const YMIN_MINUS_027: f64 = 1.1916321449683622;
    const TC_MINUS_1: f64 = 0.46163214496836225;

    let mut sign = 1;
    if x.is_nan() || x.is_infinite() {
        return (x, sign);
    }
    if x == 0.0 {
        return (f64::INFINITY, sign);
    }

    let neg = x < 0.0;
    let x = x.abs();

    if x < TINY {
        if neg {
            sign = -1;
        }
        return (-log(x), sign);
    }
    let mut nadj = 0.0;
    if neg {
        if x >= TWO52 {
            return (f64::INFINITY, sign);
        }
        let t = sin_pi::<FUSED>(x);
        if t == 0.0 {
            return (f64::INFINITY, sign);
        }
        nadj = log(PI / (t * x).abs());
        if t < 0.0 {
            sign = -1;
        }
    }

    let mut lgamma;
    if x == 1.0 || x == 2.0 {
        return (0.0, sign);
    } else if x < 2.0 {
        let (y, i);
        if x <= 0.9 {
            lgamma = -log(x);
            (y, i) = if x >= YMIN_MINUS_1_PLUS_027 {
                (1.0 - x, 0)
            } else if x >= YMIN_MINUS_1_MINUS_027 {
                (x - TC_MINUS_1, 1)
            } else {
                (x, 2)
            };
        } else {
            lgamma = 0.0;
            (y, i) = if x >= YMIN_PLUS_027 {
                (2.0 - x, 0)
            } else if x >= YMIN_MINUS_027 {
                (x - TC, 1)
            } else {
                (x - 1.0, 2)
            };
        }
        let a = &LGAM_A;
        let t = &LGAM_T;
        match i {
            0 => {
                let z = y * y;
                let p1 = madd::<FUSED>(
                    a[0],
                    z,
                    madd::<FUSED>(
                        a[2],
                        z,
                        madd::<FUSED>(
                            a[4],
                            z,
                            madd::<FUSED>(a[6], z, madd::<FUSED>(a[8], z, a[10])),
                        ),
                    ),
                );
                let p2_tail = madd::<FUSED>(
                    a[1],
                    z,
                    madd::<FUSED>(
                        a[3],
                        z,
                        madd::<FUSED>(
                            a[5],
                            z,
                            madd::<FUSED>(a[7], z, madd::<FUSED>(a[9], z, a[11])),
                        ),
                    ),
                );
                let p = madd::<FUSED>(y * p1, z, p2_tail);
                lgamma += msub::<FUSED>(p, 0.5, y);
            }
            1 => {
                let z = y * y;
                let w = z * y;
                let p1 = madd::<FUSED>(
                    t[0],
                    w,
                    madd::<FUSED>(
                        t[3],
                        w,
                        madd::<FUSED>(t[6], w, madd::<FUSED>(t[9], w, t[12])),
                    ),
                );
                let p2 = madd::<FUSED>(
                    t[1],
                    w,
                    madd::<FUSED>(
                        t[4],
                        w,
                        madd::<FUSED>(t[7], w, madd::<FUSED>(t[10], w, t[13])),
                    ),
                );
                let p3 = madd::<FUSED>(
                    t[2],
                    w,
                    madd::<FUSED>(
                        t[5],
                        w,
                        madd::<FUSED>(t[8], w, madd::<FUSED>(t[11], w, t[14])),
                    ),
                );
                let p = nmsub::<FUSED>(msub::<FUSED>(TT, w, madd::<FUSED>(p2, y, p3)), z, p1);
                lgamma += TF + p;
            }
            _ => {
                let u = &LGAM_U;
                let v = &LGAM_V;
                let p1 = y * madd::<FUSED>(
                    u[0],
                    y,
                    madd::<FUSED>(
                        u[1],
                        y,
                        madd::<FUSED>(
                            u[2],
                            y,
                            madd::<FUSED>(u[3], y, madd::<FUSED>(u[4], y, u[5])),
                        ),
                    ),
                );
                let p2 = madd::<FUSED>(
                    1.0,
                    y,
                    madd::<FUSED>(
                        v[1],
                        y,
                        madd::<FUSED>(
                            v[2],
                            y,
                            madd::<FUSED>(v[3], y, madd::<FUSED>(v[4], y, v[5])),
                        ),
                    ),
                );
                lgamma += madd::<FUSED>(p1 / p2, -0.5, y);
            }
        }
    } else if x < 8.0 {
        let s = &LGAM_S;
        let r = &LGAM_R;
        let i = x as i64;
        let y = x - i as f64;
        let p = y * madd::<FUSED>(
            s[0],
            y,
            madd::<FUSED>(
                s[1],
                y,
                madd::<FUSED>(
                    s[2],
                    y,
                    madd::<FUSED>(
                        s[3],
                        y,
                        madd::<FUSED>(s[4], y, madd::<FUSED>(s[5], y, s[6])),
                    ),
                ),
            ),
        );
        let q = madd::<FUSED>(
            1.0,
            y,
            madd::<FUSED>(
                r[1],
                y,
                madd::<FUSED>(
                    r[2],
                    y,
                    madd::<FUSED>(
                        r[3],
                        y,
                        madd::<FUSED>(r[4], y, madd::<FUSED>(r[5], y, r[6])),
                    ),
                ),
            ),
        );
        lgamma = madd::<FUSED>(p / q, 0.5, y);
        if i >= 3 {
            let mut z = 1.0;
            for k in (2..i).rev() {
                z *= y + k as f64;
            }
            lgamma += log(z);
        }
    } else if x < TWO58 {
        let w_ = &LGAM_W;
        let t = log(x);
        let z = 1.0 / x;
        let y = z * z;
        let w = madd::<FUSED>(
            w_[0],
            z,
            madd::<FUSED>(
                w_[1],
                y,
                madd::<FUSED>(
                    w_[2],
                    y,
                    madd::<FUSED>(
                        w_[3],
                        y,
                        madd::<FUSED>(w_[4], y, madd::<FUSED>(w_[5], y, w_[6])),
                    ),
                ),
            ),
        );
        lgamma = madd::<FUSED>(w, x - 0.5, t - 1.0);
    } else {
        lgamma = x * (log(x) - 1.0);
    }
    if neg {
        lgamma = nadj - lgamma;
    }
    (lgamma, sign)
}

/// Go's `sinPi`, `sin(πx)` for `x > 0`, with the sign of `Γ(-x)` folded in.
#[inline(always)]
fn sin_pi<const FUSED: bool>(mut x: f64) -> f64 {
    if x < 0.25 {
        return -sin::<FUSED>(PI * x);
    }

    let mut z = x.floor();
    let mut n: i64;
    if z == x {
        if x >= TWO53 {
            x = 0.0;
            n = 0;
        } else {
            if x < TWO52 {
                z = x + TWO52;
            }
            n = (z.to_bits() & 1) as i64;
            x = n as f64;
            n <<= 2;
        }
    } else {
        x %= 2.0;
        n = (x * 4.0) as i64;
    }
    let x = match n {
        0 => sin::<FUSED>(PI * x),
        1 | 2 => cos::<FUSED>(PI * (0.5 - x)),
        3 | 4 => sin::<FUSED>(PI * (1.0 - x)),
        5 | 6 => -cos::<FUSED>(PI * (x - 1.5)),
        _ => sin::<FUSED>(PI * (x - 2.0)),
    };
    -x
}

const PI4A: f64 = 7.85398125648498535156e-1;
const PI4B: f64 = 3.77489470793079817668e-8;
const PI4C: f64 = 2.69515142907905952645e-15;
const FOUR_OVER_PI: f64 = 1.2732395447351628;
const SIN: [f64; 6] = [
    1.58962301576546568060e-10,
    -2.50507477628578072866e-8,
    2.75573136213857245213e-6,
    -1.98412698295895385996e-4,
    8.33333333332211858878e-3,
    -1.66666666666666307295e-1,
];
const COS: [f64; 6] = [
    -1.13585365213876817300e-11,
    2.08757008419747316778e-9,
    -2.75573141792967388112e-7,
    2.48015872888517045348e-5,
    -1.38888888888730564116e-3,
    4.16666666666665929218e-2,
];

/// The octant `j` and remainder `z` of `x` in `[0, 1]`: Go's reduction for arguments
/// below `reduceThreshold`, the only ones `sin_pi` passes.
#[inline(always)]
fn reduce<const FUSED: bool>(x: f64) -> (u64, f64) {
    let mut j = (x * FOUR_OVER_PI) as u64;
    let mut y = j as f64;
    if j & 1 == 1 {
        j += 1;
        y += 1.0;
    }
    j &= 7;
    let z = msub::<FUSED>(msub::<FUSED>(msub::<FUSED>(x, y, PI4A), y, PI4B), y, PI4C);
    (j, z)
}

#[inline(always)]
fn poly<const FUSED: bool>(c: &[f64; 6], zz: f64) -> f64 {
    let mut p = c[0];
    for &k in &c[1..] {
        p = madd::<FUSED>(k, p, zz);
    }
    p
}

#[inline(always)]
fn sin_series<const FUSED: bool>(z: f64, zz: f64) -> f64 {
    madd::<FUSED>(z, z * zz, poly::<FUSED>(&SIN, zz))
}

#[inline(always)]
fn cos_series<const FUSED: bool>(zz: f64) -> f64 {
    madd::<FUSED>(
        msub::<FUSED>(1.0, 0.5, zz),
        zz * zz,
        poly::<FUSED>(&COS, zz),
    )
}

#[inline(always)]
fn sin<const FUSED: bool>(mut x: f64) -> f64 {
    if x == 0.0 {
        return x;
    }
    let mut sign = false;
    if x < 0.0 {
        x = -x;
        sign = true;
    }
    let (mut j, z) = reduce::<FUSED>(x);
    if j > 3 {
        sign = !sign;
        j -= 4;
    }
    let zz = z * z;
    let y = if j == 1 || j == 2 {
        cos_series::<FUSED>(zz)
    } else {
        sin_series::<FUSED>(z, zz)
    };
    if sign { -y } else { y }
}

#[inline(always)]
fn cos<const FUSED: bool>(x: f64) -> f64 {
    let mut sign = false;
    let (mut j, z) = reduce::<FUSED>(x.abs());
    if j > 3 {
        j -= 4;
        sign = !sign;
    }
    if j > 1 {
        sign = !sign;
    }
    let zz = z * z;
    let y = if j == 1 || j == 2 {
        sin_series::<FUSED>(z, zz)
    } else {
        cos_series::<FUSED>(zz)
    };
    if sign { -y } else { y }
}
