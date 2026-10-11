const SECOND: i64 = 1_000_000_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;

const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

/// Go's `int(x)` and `int64(x)` of a float64 on amd64: truncation toward zero, and `i64::MIN`
/// (what `CVTTSD2SQ` returns) for NaN and every value outside the int64 range. Rust's `as`
/// saturates and maps NaN to zero.
#[must_use]
pub fn f64_to_i64(x: f64) -> i64 {
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

/// Go's `time.Duration(secs * float64(time.Second))`, in nanoseconds: one float64 multiply, then
/// the amd64 conversion above.
#[must_use]
pub fn duration_from_secs_f64(secs: f64) -> i64 {
    f64_to_i64(secs * 1e9)
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
