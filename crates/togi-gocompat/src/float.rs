use std::fmt::{self, Write as _};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    E,
    UpperE,
    F,
    G,
    UpperG,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonFinite;

impl fmt::Display for NonFinite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("json: unsupported value: NaN or infinity")
    }
}

impl std::error::Error for NonFinite {}

/// Rust's formatting precision tops out at `u16::MAX`, while Go accepts any `int`. An f64's exact
/// decimal expansion has at most 767 significant digits and 1074 after the point, so asking for
/// this many is exact and every further requested digit is a zero.
const EXACT_DIGITS: usize = 1100;

struct Digits {
    digits: Vec<u8>,
    dp: i64,
}

impl Digits {
    fn parse(scientific: &str) -> Self {
        let Some((mantissa, exponent)) = scientific.split_once('e') else {
            unreachable!("Rust's {{:e}} output always contains an exponent");
        };
        let Ok(exponent) = exponent.parse::<i64>() else {
            unreachable!("Rust's {{:e}} exponent is a decimal integer");
        };
        let mut digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
        while digits.last() == Some(&b'0') {
            digits.pop();
        }
        let dp = if digits.is_empty() { 0 } else { exponent + 1 };
        Self { digits, dp }
    }

    /// Rust's shortest digits round an exact tie in the last place upward and Go's round to even,
    /// so the nearest digits at the same length replace them whenever they read back as the value.
    fn shortest(magnitude: f64) -> Self {
        let shortest = Self::parse(&format!("{magnitude:e}"));
        let significant = shortest.digits.len().max(1);
        let nearest = format!("{:.*e}", significant - 1, magnitude);
        if nearest.parse::<f64>() == Ok(magnitude) {
            Self::parse(&nearest)
        } else {
            shortest
        }
    }

    fn rounded(magnitude: f64, significant: usize) -> Self {
        let significant = significant.min(EXACT_DIGITS);
        Self::parse(&format!("{:.*e}", significant - 1, magnitude))
    }

    fn len(&self) -> i64 {
        i64::try_from(self.digits.len()).unwrap_or(i64::MAX)
    }

    fn at(&self, index: i64) -> u8 {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.digits.get(i))
            .copied()
            .unwrap_or(b'0')
    }
}

fn clamp_len(n: i64) -> usize {
    usize::try_from(n.max(0)).unwrap_or(0)
}

fn write_e(out: &mut String, d: &Digits, prec: usize, upper: bool) {
    out.push(char::from(d.at(0)));
    if prec > 0 {
        out.push('.');
        for i in 1..=prec {
            out.push(char::from(d.at(i64::try_from(i).unwrap_or(i64::MAX))));
        }
    }
    out.push(if upper { 'E' } else { 'e' });
    let exp = if d.digits.is_empty() { 0 } else { d.dp - 1 };
    out.push(if exp < 0 { '-' } else { '+' });
    let exp = exp.unsigned_abs();
    if exp < 10 {
        out.push('0');
    }
    out.push_str(&exp.to_string());
}

fn write_f(out: &mut String, d: &Digits, prec: usize) {
    if d.dp > 0 {
        for i in 0..d.dp {
            out.push(char::from(d.at(i)));
        }
    } else {
        out.push('0');
    }
    if prec > 0 {
        out.push('.');
        for i in 0..i64::try_from(prec).unwrap_or(i64::MAX) {
            let j = d.dp + i;
            out.push(if j >= 0 && j < d.len() {
                char::from(d.at(j))
            } else {
                '0'
            });
        }
    }
}

fn write_g(out: &mut String, d: &Digits, prec: i64, shortest: bool, upper: bool) {
    let mut eprec = prec;
    if eprec > d.len() && d.len() >= d.dp {
        eprec = d.len();
    }
    if shortest {
        eprec = 6;
    }
    let exp = d.dp - 1;
    if exp < -4 || exp >= eprec {
        let prec = prec.min(d.len());
        write_e(out, d, clamp_len(prec - 1), upper);
    } else {
        write_f(out, d, clamp_len(d.len() - d.dp));
    }
}

/// Formats like Go's `strconv.FormatFloat(v, verb, prec, 64)`; `None` is Go's precision -1, the
/// shortest representation that round-trips.
pub fn format_float(v: f64, verb: Verb, prec: Option<usize>) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 { "+Inf" } else { "-Inf" }.to_owned();
    }
    let mut out = String::new();
    if v.is_sign_negative() {
        out.push('-');
    }
    let magnitude = v.abs();
    let upper = matches!(verb, Verb::UpperE | Verb::UpperG);
    match (verb, prec) {
        (Verb::F, Some(prec)) => {
            let exact = prec.min(EXACT_DIGITS);
            write!(out, "{magnitude:.exact$}").ok();
            out.extend(std::iter::repeat_n('0', prec - exact));
        }
        (Verb::F, None) => {
            let d = Digits::shortest(magnitude);
            write_f(&mut out, &d, clamp_len(d.len() - d.dp));
        }
        (Verb::E | Verb::UpperE, None) => {
            let d = Digits::shortest(magnitude);
            write_e(&mut out, &d, clamp_len(d.len() - 1), upper);
        }
        (Verb::E | Verb::UpperE, Some(prec)) => {
            let d = Digits::rounded(magnitude, prec + 1);
            write_e(&mut out, &d, prec, upper);
        }
        (Verb::G | Verb::UpperG, None) => {
            let d = Digits::shortest(magnitude);
            write_g(&mut out, &d, d.len(), true, upper);
        }
        (Verb::G | Verb::UpperG, Some(prec)) => {
            let prec = prec.max(1);
            let d = Digits::rounded(magnitude, prec);
            write_g(
                &mut out,
                &d,
                i64::try_from(prec).unwrap_or(i64::MAX),
                false,
                upper,
            );
        }
    }
    out
}

/// Go's `%v` of a float64, which is `%g` with the shortest representation.
pub fn format_v(v: f64) -> String {
    format_float(v, Verb::G, None)
}

/// Go's `encoding/json` encoding of a float64: plain decimal between 1e-6 and 1e21, otherwise
/// exponent form with the single-digit negative exponent's leading zero removed.
pub fn json_f64(v: f64) -> Result<String, NonFinite> {
    if !v.is_finite() {
        return Err(NonFinite);
    }
    let magnitude = v.abs();
    if magnitude != 0.0 && !(1e-6..1e21).contains(&magnitude) {
        let mut out = format_float(v, Verb::E, None);
        let bytes = out.as_bytes();
        let n = bytes.len();
        if n >= 4 && bytes[n - 4] == b'e' && bytes[n - 3] == b'-' && bytes[n - 2] == b'0' {
            out.remove(n - 2);
        }
        return Ok(out);
    }
    Ok(format_float(v, Verb::F, None))
}
