use std::fs;
use std::path::Path;

use super::{f64_to_i64_amd64, f64_to_i64_arm64};

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

fn int(text: &str) -> i64 {
    text.parse().unwrap()
}

fn expect_none(kind: &str, failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{kind}: {} mismatches, first {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}

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
fn amd64_float_to_int64_matches_go() {
    check_float_to_int("convert-amd64.txt", f64_to_i64_amd64);
}

#[test]
fn arm64_float_to_int64_matches_go() {
    check_float_to_int("convert-arm64.txt", f64_to_i64_arm64);
}

#[test]
fn amd64_float_to_int64_boundaries() {
    let f = f64_to_i64_amd64;
    assert_eq!(f(f64::NAN), i64::MIN);
    assert_eq!(f(f64::INFINITY), i64::MIN);
    assert_eq!(f(f64::NEG_INFINITY), i64::MIN);
    assert_eq!(f(9_223_372_036_854_775_808.0), i64::MIN);
    assert_eq!(f(9_223_372_036_854_774_784.0), 9_223_372_036_854_774_784);
    assert_eq!(f(-9_223_372_036_854_775_808.0), i64::MIN);
    assert_eq!(f(-9_223_372_036_854_777_856.0), i64::MIN);
    assert_eq!(f(-9_223_372_036_854_774_784.0), -9_223_372_036_854_774_784);
    assert_eq!(f(-0.9), 0);
    assert_eq!(f(-1.9), -1);
    assert_eq!(f(2.9), 2);
}

#[test]
fn arm64_float_to_int64_boundaries() {
    let f = f64_to_i64_arm64;
    assert_eq!(f(f64::NAN), 0);
    assert_eq!(f(f64::INFINITY), i64::MAX);
    assert_eq!(f(f64::NEG_INFINITY), i64::MIN);
    assert_eq!(f(9_223_372_036_854_775_808.0), i64::MAX);
    assert_eq!(f(9_223_372_036_854_774_784.0), 9_223_372_036_854_774_784);
    assert_eq!(f(-9_223_372_036_854_775_808.0), i64::MIN);
    assert_eq!(f(-9_223_372_036_854_777_856.0), i64::MIN);
    assert_eq!(f(-9_223_372_036_854_774_784.0), -9_223_372_036_854_774_784);
    assert_eq!(f(1e10 * 1e9), 9_223_372_036_854_775_807);
    assert_eq!(f(-0.9), 0);
    assert_eq!(f(-1.9), -1);
    assert_eq!(f(2.9), 2);
}
