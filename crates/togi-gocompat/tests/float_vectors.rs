#![cfg(test)]

use std::fs;
use std::path::Path;

use togi_gocompat::float::{Verb, format_float, format_v, json_f64};

fn lines(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(name);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.ends_with('\n'), "{name} ends without a newline");
    text.lines().map(str::to_owned).collect()
}

fn value(bits: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(bits, 16).unwrap())
}

fn verb(code: &str) -> Verb {
    match code {
        "e" => Verb::E,
        "E" => Verb::UpperE,
        "f" => Verb::F,
        "g" => Verb::G,
        "G" => Verb::UpperG,
        other => panic!("unknown verb {other}"),
    }
}

fn check_bits_file(name: &str, format: impl Fn(f64) -> String) {
    let lines = lines(name);
    assert!(lines.len() > 10_000, "{name} has {} vectors", lines.len());
    let mut failures = Vec::new();
    for line in &lines {
        let (bits, want) = line.split_once('\t').unwrap();
        let got = format(value(bits));
        if got != want {
            failures.push(format!("{bits}: got {got:?}, want {want:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{name}: {} mismatches, first {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}

#[test]
fn json_vectors_match_go() {
    check_bits_file("float-json.txt", |v| {
        json_f64(v).unwrap_or_else(|_| "error".to_owned())
    });
}

#[test]
fn verb_v_vectors_match_go() {
    check_bits_file("float-v.txt", format_v);
}

#[test]
fn format_vectors_match_go() {
    let lines = lines("float-format.txt");
    assert!(lines.len() > 100_000, "{} vectors", lines.len());
    let mut failures = Vec::new();
    for line in &lines {
        let mut fields = line.split('\t');
        let (Some(code), Some(prec), Some(bits), Some(want), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            panic!("malformed vector line {line:?}");
        };
        let prec = match prec.parse::<i32>().unwrap() {
            -1 => None,
            p => Some(usize::try_from(p).unwrap()),
        };
        let got = format_float(value(bits), verb(code), prec);
        if got != want {
            failures.push(format!(
                "{code} {prec:?} {bits}: got {got:?}, want {want:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches, first {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}

#[test]
fn json_boundaries_read_as_go_writes_them() {
    let cases: [(f64, &str); 12] = [
        (1.0, "1"),
        (0.1, "0.1"),
        (1e-6, "0.000001"),
        (1e-7, "1e-7"),
        (1e20, "100000000000000000000"),
        (1e21, "1e+21"),
        (-1.5, "-1.5"),
        (-0.0, "-0"),
        (5e-324, "5e-324"),
        (f64::MAX, "1.7976931348623157e+308"),
        (f64::MIN_POSITIVE, "2.2250738585072014e-308"),
        (123_456_789.5, "123456789.5"),
    ];
    for (v, want) in cases {
        assert_eq!(json_f64(v).unwrap(), want, "{v:e}");
    }
    assert!(json_f64(f64::NAN).is_err());
    assert!(json_f64(f64::INFINITY).is_err());
    assert!(json_f64(f64::NEG_INFINITY).is_err());
}

#[test]
fn verb_v_boundaries_read_as_go_writes_them() {
    assert_eq!(format_v(1e-5), "1e-05");
    assert_eq!(format_v(1e-7), "1e-07");
    assert_eq!(format_v(1e20), "1e+20");
    assert_eq!(format_v(123_456_789.5), "1.234567895e+08");
    assert_eq!(format_v(f64::NAN), "NaN");
    assert_eq!(format_v(f64::INFINITY), "+Inf");
    assert_eq!(format_v(f64::NEG_INFINITY), "-Inf");
}

/// Go's output at a precision past the exact expansion is that expansion followed by zeros, so
/// the expectation is the Rust output at a precision below Rust's limit with the zeros inserted.
fn zero_padded(v: f64, verb: Verb, prec: usize) -> String {
    let base = if verb == Verb::F { 1100 } else { 1000 };
    let shorter = format_float(v, verb, Some(base));
    let zeros = "0".repeat(prec - base);
    match shorter.find(['e', 'E']) {
        Some(at) => format!("{}{zeros}{}", &shorter[..at], &shorter[at..]),
        None => format!("{shorter}{zeros}"),
    }
}

#[test]
fn precision_beyond_rust_limit_matches_go() {
    let tenth = "1.000000000000000055511151231257827021181583404541015625";
    let zeros = |n: usize| "0".repeat(n);
    assert_eq!(
        format_float(0.0, Verb::E, Some(65536)),
        format!("0.{}e+00", zeros(65536))
    );
    assert_eq!(
        format_float(1.0, Verb::UpperE, Some(100_000)),
        format!("1.{}E+00", zeros(100_000))
    );
    assert_eq!(
        format_float(0.1, Verb::E, Some(65536)),
        format!("{tenth}{}e-01", zeros(65536 - 54))
    );
    assert_eq!(
        format_float(1.0, Verb::F, Some(65536)),
        format!("1.{}", zeros(65536))
    );
    assert_eq!(
        format_float(0.0, Verb::F, Some(100_000)),
        format!("0.{}", zeros(100_000))
    );
    for v in [0.0, 1.0, 0.1, 5e-324, f64::MAX] {
        for verb in [Verb::E, Verb::UpperE, Verb::F] {
            for prec in [65536, 100_000] {
                assert_eq!(
                    format_float(v, verb, Some(prec)),
                    zero_padded(v, verb, prec),
                    "{v:e} {verb:?} {prec}"
                );
            }
        }
    }
    assert_eq!(format_float(0.0, Verb::UpperG, Some(65537)), "0");
    assert_eq!(format_float(1.5, Verb::UpperG, Some(65537)), "1.5");
    assert_eq!(
        format_float(0.1, Verb::G, Some(65537)),
        "0.1000000000000000055511151231257827021181583404541015625"
    );
}
