#![cfg(test)]

use std::fs;
use std::path::Path;

use togi_gocompat::convert::{
    ceil, duration_from_secs_f64, duration_hours, duration_minutes, duration_round,
    duration_seconds, duration_string, duration_truncate, f64_to_i64, floor, parse_duration, round,
    shl_i64, trunc,
};

const NATIVE: &str = if cfg!(target_arch = "x86_64") {
    "convert-amd64.txt"
} else {
    "convert-arm64.txt"
};

fn rows(name: &str, kind: &str) -> Vec<Vec<String>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(name);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.ends_with('\n'), "{name} ends without a newline");
    let rows: Vec<Vec<String>> = text
        .lines()
        .map(|line| line.split('\t').map(str::to_owned).collect::<Vec<_>>())
        .filter(|fields| fields[0] == kind)
        .map(|fields| fields[1..].to_vec())
        .collect();
    assert!(!rows.is_empty(), "{name} has no {kind} vectors");
    rows
}

fn f64_of(bits: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(bits, 16).unwrap())
}

fn f32_of(bits: &str) -> f32 {
    f32::from_bits(u32::from_str_radix(bits, 16).unwrap())
}

fn int(text: &str) -> i64 {
    text.parse().unwrap()
}

fn same_f64(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits()
}

fn same_f32(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits()
}

fn hex_bytes(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0, "odd hex {text:?}");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn expect_none(kind: &str, failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{kind}: {} mismatches, first {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}

/// The native conversion, including the scaled duration conversions, against Go on this target.
fn check_float_to_int(name: &str, convert: fn(f64) -> i64) {
    let mut failures = Vec::new();
    for row in rows(name, "i64") {
        let got = convert(f64_of(&row[0]));
        if got != int(&row[1]) {
            failures.push(format!("{}: got {got}, want {}", row[0], row[1]));
        }
    }
    for row in rows(name, "dur-secs") {
        let got = convert(f64_of(&row[0]) * 1e9);
        if got != int(&row[1]) {
            failures.push(format!("{} s: got {got}, want {}", row[0], row[1]));
        }
    }
    for row in rows(name, "dur-scaled") {
        let got = convert(f64_of(&row[0]) * f64_of(&row[1]));
        if got != int(&row[2]) {
            failures.push(format!(
                "{} * {}: got {got}, want {}",
                row[0], row[1], row[2]
            ));
        }
    }
    expect_none(name, &failures);
}

#[test]
fn native_float_to_int64_matches_go() {
    check_float_to_int(NATIVE, f64_to_i64);
}

#[test]
fn native_duration_from_seconds_follows_the_target() {
    assert_eq!(duration_from_secs_f64(1.5), 1_500_000_000);
    let overflow = if cfg!(target_arch = "x86_64") {
        i64::MIN
    } else {
        i64::MAX
    };
    assert_eq!(duration_from_secs_f64(1e10), overflow);
    let nan = if cfg!(target_arch = "x86_64") {
        i64::MIN
    } else {
        0
    };
    assert_eq!(duration_from_secs_f64(f64::NAN), nan);
}

#[test]
fn float32_rounding_equals_rust_as_f32() {
    let mut failures = Vec::new();
    for row in rows("convert.txt", "f32")
        .into_iter()
        .chain(rows(NATIVE, "f32"))
    {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the vector is the narrowing itself"
        )]
        let got = f64_of(&row[0]) as f32;
        if !same_f32(got, f32_of(&row[1])) {
            failures.push(format!(
                "{}: got {:08x}, want {}",
                row[0],
                got.to_bits(),
                row[1]
            ));
        }
    }
    expect_none("f32", &failures);
}

#[test]
fn float32_arithmetic_equals_rust() {
    let mut failures = Vec::new();
    for row in rows("convert.txt", "f32-ops")
        .into_iter()
        .chain(rows(NATIVE, "f32-ops"))
    {
        let (a, b) = (f32_of(&row[0]), f32_of(&row[1]));
        let ok = same_f32(a + b, f32_of(&row[2]))
            && same_f32(a - b, f32_of(&row[3]))
            && same_f32(a * b, f32_of(&row[4]))
            && same_f32(a / b, f32_of(&row[5]))
            && same_f64(f64::from(a), f64_of(&row[6]))
            && (f64::from(a) > f64::from(b)) == (row[7] == "true")
            && (a > b) == (row[8] == "true");
        if !ok {
            failures.push(format!("{} {}", row[0], row[1]));
        }
    }
    expect_none("f32-ops", &failures);
}

#[test]
fn rounding_functions_match_go() {
    let mut failures = Vec::new();
    for row in rows("convert.txt", "math")
        .into_iter()
        .chain(rows(NATIVE, "math"))
    {
        let x = f64_of(&row[0]);
        let ok = same_f64(round(x), f64_of(&row[1]))
            && same_f64(floor(x), f64_of(&row[2]))
            && same_f64(ceil(x), f64_of(&row[3]))
            && same_f64(trunc(x), f64_of(&row[4]));
        if !ok {
            failures.push(row[0].clone());
        }
    }
    expect_none("math", &failures);
}

#[test]
fn duration_strings_match_go() {
    let mut failures = Vec::new();
    for row in rows("convert.txt", "dur-string") {
        let got = duration_string(int(&row[0]));
        if got != row[1] {
            failures.push(format!("{}: got {got:?}, want {:?}", row[0], row[1]));
        }
    }
    expect_none("dur-string", &failures);
}

#[test]
fn duration_unit_conversions_match_go() {
    let mut failures = Vec::new();
    for row in rows("convert.txt", "dur-float") {
        let d = int(&row[0]);
        let ok = same_f64(duration_seconds(d), f64_of(&row[1]))
            && same_f64(duration_minutes(d), f64_of(&row[2]))
            && same_f64(duration_hours(d), f64_of(&row[3]));
        if !ok {
            failures.push(row[0].clone());
        }
    }
    for row in rows("convert.txt", "dur-int") {
        let d = int(&row[0]);
        if d / 1_000_000 != int(&row[1]) || d / 1_000 != int(&row[2]) {
            failures.push(row[0].clone());
        }
    }
    expect_none("dur-float", &failures);
}

#[test]
fn duration_rounding_matches_go() {
    let mut failures = Vec::new();
    for row in rows("convert.txt", "dur-round") {
        let (d, m) = (int(&row[0]), int(&row[1]));
        if duration_round(d, m) != int(&row[2]) || duration_truncate(d, m) != int(&row[3]) {
            failures.push(format!("{d} {m}"));
        }
    }
    expect_none("dur-round", &failures);
}

#[test]
fn duration_multiplication_wraps_as_go() {
    let mut failures = Vec::new();
    for row in rows("convert.txt", "dur-mul") {
        if int(&row[0]).wrapping_mul(int(&row[1])) != int(&row[2]) {
            failures.push(format!("{} * {}", row[0], row[1]));
        }
    }
    expect_none("dur-mul", &failures);
}

#[test]
fn integer_division_and_remainder_follow_go() {
    let mut failures = Vec::new();
    for row in rows("convert.txt", "divmod") {
        let (a, b) = (int(&row[0]), int(&row[1]));
        if a.wrapping_div(b) != int(&row[2]) || a.wrapping_rem(b) != int(&row[3]) {
            failures.push(format!("{a} {b}"));
        }
    }
    expect_none("divmod", &failures);
    assert_eq!((-7_i64).wrapping_div(2), -3);
    assert_eq!((-7_i64).wrapping_rem(2), -1);
    assert_eq!(i64::MIN.wrapping_div(-1), i64::MIN);
    assert_eq!(i64::MIN.wrapping_rem(-1), 0);
}

#[test]
fn shifts_follow_go() {
    let mut failures = Vec::new();
    for row in rows("convert.txt", "shl") {
        let n: u64 = row[1].parse().unwrap();
        if shl_i64(int(&row[0]), n) != int(&row[2]) {
            failures.push(format!("{} << {n}", row[0]));
        }
    }
    for row in rows("convert.txt", "shl-int") {
        let n: u64 = row[1].parse().unwrap();
        if shl_i64(int(&row[0]), n) != int(&row[2]) {
            failures.push(format!("{} << {n} (int)", row[0]));
        }
    }
    expect_none("shl", &failures);
}

#[test]
fn duration_parsing_matches_go() {
    let mut failures = Vec::new();
    let mut parsed = 0;
    for row in rows("convert.txt", "dur-parse") {
        let want = (row[1] != "error").then(|| int(&row[1]));
        let got = String::from_utf8(hex_bytes(&row[0])).map_or(None, |text| parse_duration(&text));
        parsed += usize::from(got.is_some());
        if got != want {
            failures.push(format!("{}: got {got:?}, want {want:?}", row[0]));
        }
    }
    expect_none("dur-parse", &failures);
    assert!(parsed > 400, "only {parsed} inputs parse");
}

#[test]
fn duration_parsing_hand_written() {
    assert_eq!(parse_duration("0.5ns0.5ns"), Some(0));
    assert_eq!(parse_duration("1.5s"), Some(1_500_000_000));
    assert_eq!(parse_duration("-0"), Some(0));
    assert_eq!(parse_duration("1h2m3s"), Some(3_723_000_000_000));
    assert_eq!(parse_duration("12\u{b5}s"), Some(12_000));
    assert_eq!(parse_duration("12\u{3bc}s"), Some(12_000));
    assert_eq!(parse_duration("-9223372036854775808ns"), Some(i64::MIN));
    assert_eq!(parse_duration("9223372036854775807ns"), Some(i64::MAX));
    assert_eq!(parse_duration("9223372036854775808ns"), None);
    for bad in ["", "+", "-", ".s", "1.s2", "1", "1d", "s", "1 s"] {
        assert_eq!(parse_duration(bad), None, "{bad:?}");
    }
    assert_eq!(parse_duration("1.s"), Some(1_000_000_000));
    assert_eq!(parse_duration(".5s"), Some(500_000_000));
}
