use std::fs;
use std::path::Path;

use super::{
    ceil_asm, ceil_rounding_instruction, f64_to_i64_amd64, f64_to_i64_arm64, floor_asm,
    floor_rounding_instruction, trunc_asm, trunc_rounding_instruction,
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

fn bits_of(text: &str) -> u64 {
    u64::from_str_radix(text, 16).unwrap()
}

/// Go's assembly `Floor`, `Ceil` and `Trunc` for a CPU without SSE4.1, from a run with
/// `GODEBUG=cpu.sse41=off`, and the same on convert.txt's rows, which hold no NaN and are
/// what either branch gives.
#[test]
fn asm_rounding_matches_go_without_sse4_1() {
    let mut failures = Vec::new();
    let mut nans = 0;
    for row in rows("convert-amd64-nosse41.txt", "floor") {
        let x = f64_of(&row[0]);
        nans += usize::from(x.is_nan());
        let got = [floor_asm(x), ceil_asm(x), trunc_asm(x)].map(f64::to_bits);
        let want = [&row[1], &row[2], &row[3]].map(|bits| bits_of(bits));
        if got != want {
            failures.push(format!("{}: got {got:x?}, want {want:x?}", row[0]));
        }
    }
    for row in rows("convert.txt", "math") {
        let x = f64_of(&row[0]);
        let got = [floor_asm(x), ceil_asm(x), trunc_asm(x)].map(f64::to_bits);
        let want = [&row[2], &row[3], &row[4]].map(|bits| bits_of(bits));
        if got != want {
            failures.push(format!(
                "convert.txt {}: got {got:x?}, want {want:x?}",
                row[0]
            ));
        }
    }
    expect_none("asm rounding", &failures);
    assert!(nans >= 5, "only {nans} NaN inputs");
}

/// `ROUNDSD` and `FRINTMD`, `FRINTPD`, `FRINTZD`: the rounding the other targets' vectors hold.
#[test]
fn rounding_instruction_matches_go() {
    let mut failures = Vec::new();
    let native = if cfg!(target_arch = "x86_64") {
        "convert-amd64.txt"
    } else {
        "convert-arm64.txt"
    };
    for row in rows("convert.txt", "math")
        .into_iter()
        .chain(rows(native, "math"))
    {
        let x = f64_of(&row[0]);
        let got = [
            floor_rounding_instruction(x),
            ceil_rounding_instruction(x),
            trunc_rounding_instruction(x),
        ]
        .map(f64::to_bits);
        let want = [&row[2], &row[3], &row[4]].map(|bits| bits_of(bits));
        if got != want {
            failures.push(format!("{}: got {got:x?}, want {want:x?}", row[0]));
        }
    }
    expect_none("rounding instruction", &failures);
}

#[test]
fn asm_rounding_hand_written() {
    let snan = f64::from_bits(0x7ff0_0000_0000_0001);
    for f in [floor_asm, ceil_asm, trunc_asm] {
        assert_eq!(f(snan).to_bits(), snan.to_bits());
        assert_eq!(f(-0.0).to_bits(), (-0.0_f64).to_bits());
        assert_eq!(f(4_503_599_627_370_496.0), 4_503_599_627_370_496.0);
    }
    assert_eq!(floor_asm(-0.5), -1.0);
    assert_eq!(floor_asm(0.5).to_bits(), 0.0_f64.to_bits());
    assert_eq!(ceil_asm(-0.5).to_bits(), (-0.0_f64).to_bits());
    assert_eq!(ceil_asm(0.5), 1.0);
    assert_eq!(trunc_asm(-0.5).to_bits(), (-0.0_f64).to_bits());
    assert_eq!(floor_asm(-2.5), -3.0);
    assert_eq!(ceil_asm(-2.5), -2.0);
    assert_eq!(trunc_asm(-2.5), -2.0);
    assert_eq!(
        floor_rounding_instruction(snan).to_bits(),
        0x7ff8_0000_0000_0001
    );
}
