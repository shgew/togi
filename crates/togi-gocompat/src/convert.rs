const SECOND: i64 = 1_000_000_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;

#[cfg(any(target_arch = "x86_64", test))]
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

/// Go's `int(x)` and `int64(x)` of a float64 on the target this crate is built for: the amd64
/// conversion on `x86_64` and the arm64 one on `aarch64`.
#[cfg(target_arch = "x86_64")]
#[must_use]
pub fn f64_to_i64(x: f64) -> i64 {
    f64_to_i64_amd64(x)
}

/// Go's `int(x)` and `int64(x)` of a float64 on the target this crate is built for: the amd64
/// conversion on `x86_64` and the arm64 one on `aarch64`.
#[cfg(target_arch = "aarch64")]
#[must_use]
pub fn f64_to_i64(x: f64) -> i64 {
    f64_to_i64_arm64(x)
}

/// Go's float64 to int64 conversion as amd64 compiles it: truncation toward zero, and `i64::MIN`
/// (what `CVTTSD2SQ` returns) for NaN and every value outside the int64 range. Rust's `as`
/// saturates and maps NaN to zero.
#[cfg(any(target_arch = "x86_64", test))]
fn f64_to_i64_amd64(x: f64) -> i64 {
    if x.is_nan() || !(-TWO_POW_63..TWO_POW_63).contains(&x) {
        return i64::MIN;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "x is inside the int64 range, so truncation toward zero is exact"
    )]
    let truncated = x as i64;
    truncated
}

/// Go's float64 to int64 conversion as arm64 compiles it (`FCVTZSD`): truncation toward zero, zero
/// for NaN, and saturation at `i64::MIN` and `i64::MAX`. This is Rust's `as`.
#[cfg(any(target_arch = "aarch64", test))]
fn f64_to_i64_arm64(x: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "saturation and a zero for NaN are the arm64 semantics"
    )]
    let converted = x as i64;
    converted
}

/// Go's `time.Duration(secs * float64(time.Second))`, in nanoseconds: one float64 multiply, then
/// the conversion above.
#[must_use]
pub fn duration_from_secs_f64(secs: f64) -> i64 {
    f64_to_i64(secs * 1e9)
}

/// Go's `math.Round`, bit for bit, including the payload of a NaN: Rust's `f64::round` quiets a
/// signalling NaN where Go's pure-Go algorithm on amd64 returns it unchanged. On arm64 Go
/// intrinsifies `Round` to `FRINTAD`, which is what `f64::round` lowers to.
///
/// `Floor`, `Ceil` and `Trunc` are [`floor`], [`ceil`] and [`trunc`], which differ from Rust's
/// only in that they quiet a signalling NaN.
#[cfg(target_arch = "x86_64")]
#[must_use]
pub fn round(x: f64) -> f64 {
    const SHIFT: u32 = 52;
    const BIAS: u32 = 1023;
    const SIGN_MASK: u64 = 1 << 63;
    const ONE: u64 = 0x3FF0_0000_0000_0000;
    const FRAC_MASK: u64 = (1 << SHIFT) - 1;
    let mut bits = x.to_bits();
    let mut e = (bits >> SHIFT) as u32 & 0x7ff;
    if e < BIAS {
        bits &= SIGN_MASK;
        if e == BIAS - 1 {
            bits |= ONE;
        }
    } else if e < BIAS + SHIFT {
        const HALF: u64 = 1 << (SHIFT - 1);
        e -= BIAS;
        bits += HALF >> e;
        bits &= !(FRAC_MASK >> e);
    }
    f64::from_bits(bits)
}

/// Go's `math.Round` on arm64; see the amd64 version.
#[cfg(target_arch = "aarch64")]
#[must_use]
pub fn round(x: f64) -> f64 {
    x.round()
}

const QUIET_NAN_BIT: u64 = 1 << 51;

fn quieted(x: f64) -> f64 {
    if x.is_nan() {
        f64::from_bits(x.to_bits() | QUIET_NAN_BIT)
    } else {
        x
    }
}

/// Go's `math.Floor` on the targets the vectors cover: `ROUNDSD` on amd64 at `GOAMD64=v1` when
/// the CPU has SSE4.1, `FRINTMD` on arm64. Both quiet a signalling NaN, which Rust's `f64::floor`
/// does not on `x86_64`. Without SSE4.1 Go runs assembly that returns a signalling NaN unchanged;
/// the vectors, and this function, assume a CPU with SSE4.1.
#[must_use]
pub fn floor(x: f64) -> f64 {
    quieted(x.floor())
}

/// Go's `math.Ceil`; see [`floor`].
#[must_use]
pub fn ceil(x: f64) -> f64 {
    quieted(x.ceil())
}

/// Go's `math.Trunc`; see [`floor`].
#[must_use]
pub fn trunc(x: f64) -> f64 {
    quieted(x.trunc())
}

const MAX_DURATION_MAGNITUDE: u64 = 1 << 63;

/// Go's `time.ParseDuration` in nanoseconds, `None` where Go returns an error (its text is
/// not reproduced; `flag` maps every error to one message). Go strings are bytes and this takes a
/// `&str`: a string that is not UTF-8 never parses in Go either, because its bytes fit no
/// digit, sign or unit.
///
/// Each fraction goes through float64 as Go does it, so `0.5ns0.5ns` is zero.
#[must_use]
pub fn parse_duration(input: &str) -> Option<i64> {
    let mut s = input.as_bytes();
    let mut d: u64 = 0;
    let mut neg = false;
    if let Some(&c) = s.first()
        && (c == b'-' || c == b'+')
    {
        neg = c == b'-';
        s = &s[1..];
    }
    if s == b"0" {
        return Some(0);
    }
    if s.is_empty() {
        return None;
    }
    while !s.is_empty() {
        if !(s[0] == b'.' || s[0].is_ascii_digit()) {
            return None;
        }
        let before = s.len();
        let (mut v, rest) = leading_int(s)?;
        s = rest;
        let pre = before != s.len();
        let mut f = 0;
        let mut scale = 1.0;
        let mut post = false;
        if s.first() == Some(&b'.') {
            s = &s[1..];
            let before = s.len();
            (f, scale, s) = leading_fraction(s);
            post = before != s.len();
        }
        if !pre && !post {
            return None;
        }
        let unit_len = s
            .iter()
            .position(|&c| c == b'.' || c.is_ascii_digit())
            .unwrap_or(s.len());
        if unit_len == 0 {
            return None;
        }
        let (name, rest) = s.split_at(unit_len);
        s = rest;
        let unit = unit_nanoseconds(name)?;
        if v > MAX_DURATION_MAGNITUDE / unit {
            return None;
        }
        v *= unit;
        if f > 0 {
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "Go's float64 detour for a fraction, in range by its own bound of 3.6e12"
            )]
            let fraction = (f as f64 * (unit as f64 / scale)) as u64;
            v += fraction;
            if v > MAX_DURATION_MAGNITUDE {
                return None;
            }
        }
        d += v;
        if d > MAX_DURATION_MAGNITUDE {
            return None;
        }
    }
    if neg {
        return Some(d.cast_signed().wrapping_neg());
    }
    if d > i64::MAX.cast_unsigned() {
        return None;
    }
    Some(d.cast_signed())
}

/// `us` spelled with U+00B5 (micro sign) and with U+03BC (Greek mu), both in Go's `unitMap`.
const MICRO_SIGN_US: &[u8] = "\u{b5}s".as_bytes();
const GREEK_MU_US: &[u8] = "\u{3bc}s".as_bytes();

fn unit_nanoseconds(name: &[u8]) -> Option<u64> {
    match name {
        b"ns" => Some(1),
        b"us" | MICRO_SIGN_US | GREEK_MU_US => Some(1_000),
        b"ms" => Some(1_000_000),
        b"s" => Some(1_000_000_000),
        b"m" => Some(60_000_000_000),
        b"h" => Some(3_600_000_000_000),
        _ => None,
    }
}

/// Go's `leadingInt`: the digits at the front, `None` where they overflow past `1<<63`.
fn leading_int(s: &[u8]) -> Option<(u64, &[u8])> {
    let mut x: u64 = 0;
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        if x > MAX_DURATION_MAGNITUDE / 10 {
            return None;
        }
        x = x * 10 + u64::from(s[i] - b'0');
        if x > MAX_DURATION_MAGNITUDE {
            return None;
        }
        i += 1;
    }
    Some((x, &s[i..]))
}

/// Go's `leadingFraction`: the digits after the point as an integer and the power of ten they
/// divide by; digits past what fits are consumed and ignored.
fn leading_fraction(s: &[u8]) -> (u64, f64, &[u8]) {
    let mut x: u64 = 0;
    let mut scale = 1.0;
    let mut overflow = false;
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        if !overflow {
            if x > (MAX_DURATION_MAGNITUDE - 1) / 10 {
                overflow = true;
            } else {
                let y = x * 10 + u64::from(s[i] - b'0');
                if y > MAX_DURATION_MAGNITUDE {
                    overflow = true;
                } else {
                    x = y;
                    scale *= 10.0;
                }
            }
        }
        i += 1;
    }
    (x, scale, &s[i..])
}

/// `Duration.Seconds`: whole seconds and the remainder are converted apart, which differs from
/// dividing the nanosecond count as a float.
#[must_use]
pub fn duration_seconds(d: i64) -> f64 {
    fraction(d, SECOND, 1e9)
}

#[must_use]
pub fn duration_minutes(d: i64) -> f64 {
    fraction(d, MINUTE, 60.0 * 1e9)
}

#[must_use]
pub fn duration_hours(d: i64) -> f64 {
    fraction(d, HOUR, 60.0 * 60.0 * 1e9)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Go converts the int64 quotient and remainder to float64 the same way"
)]
fn fraction(d: i64, unit: i64, divisor: f64) -> f64 {
    (d / unit) as f64 + (d % unit) as f64 / divisor
}

/// `Duration.Round`: halves round away from zero and a result past the int64 range saturates.
#[must_use]
pub fn duration_round(d: i64, m: i64) -> i64 {
    if m <= 0 {
        return d;
    }
    let mut r = d % m;
    if d < 0 {
        r = -r;
        if less_than_half(r, m) {
            return d + r;
        }
        let d1 = d.wrapping_sub(m).wrapping_add(r);
        if d1 < d {
            return d1;
        }
        return i64::MIN;
    }
    if less_than_half(r, m) {
        return d - r;
    }
    let d1 = d.wrapping_add(m).wrapping_sub(r);
    if d1 > d {
        return d1;
    }
    i64::MAX
}

/// `Duration.Truncate`.
#[must_use]
pub fn duration_truncate(d: i64, m: i64) -> i64 {
    if m <= 0 {
        return d;
    }
    d - d % m
}

fn less_than_half(x: i64, y: i64) -> bool {
    x.cast_unsigned().wrapping_add(x.cast_unsigned()) < y.cast_unsigned()
}

/// `Duration.String`, for the elapsed times the tools print.
#[must_use]
pub fn duration_string(d: i64) -> String {
    let neg = d < 0;
    let mut u = d.unsigned_abs();
    let mut out: Vec<u8> = Vec::with_capacity(32);
    if u < 1_000_000_000 {
        out.push(b's');
        let prec = match u {
            0 => {
                return "0s".to_owned();
            }
            1..1_000 => {
                out.push(b'n');
                0
            }
            1_000..1_000_000 => {
                out.extend("µ".bytes().rev());
                3
            }
            _ => {
                out.push(b'm');
                6
            }
        };
        u = push_fraction(&mut out, u, prec);
        push_int(&mut out, u);
    } else {
        out.push(b's');
        u = push_fraction(&mut out, u, 9);
        push_int(&mut out, u % 60);
        u /= 60;
        if u > 0 {
            out.push(b'm');
            push_int(&mut out, u % 60);
            u /= 60;
            if u > 0 {
                out.push(b'h');
                push_int(&mut out, u);
            }
        }
    }
    if neg {
        out.push(b'-');
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn push_fraction(out: &mut Vec<u8>, mut v: u64, prec: usize) -> u64 {
    let mut print = false;
    for _ in 0..prec {
        let digit = v % 10;
        print = print || digit != 0;
        if print {
            out.push(b'0' + u8::try_from(digit).unwrap_or(0));
        }
        v /= 10;
    }
    if print {
        out.push(b'.');
    }
    v
}

fn push_int(out: &mut Vec<u8>, mut v: u64) {
    if v == 0 {
        out.push(b'0');
        return;
    }
    while v > 0 {
        out.push(b'0' + u8::try_from(v % 10).unwrap_or(0));
        v /= 10;
    }
}

/// Go's `x << n` for an int64 and any unsigned count: a count of 64 or more shifts every bit out.
#[must_use]
pub fn shl_i64(x: i64, n: u64) -> i64 {
    match u32::try_from(n) {
        Ok(n) if n < 64 => x << n,
        _ => 0,
    }
}

#[cfg(test)]
#[path = "convert_tests.rs"]
mod tests;
