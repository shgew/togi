use super::{frexp, is_odd_int, ldexp, modf, pow};
use crate::math::{arm64, go};

const ARM64: &str = include_str!("../../../testdata/pow-arm64.txt");

struct Vector {
    base: u64,
    exponent: u64,
    want: u64,
}

fn parse(text: &'static str) -> Vec<Vector> {
    assert!(text.ends_with('\n'));
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split(' ');
            assert_eq!(fields.next(), Some("pow"));
            let mut hex = || u64::from_str_radix(fields.next().unwrap(), 16).unwrap();
            let (base, exponent, want) = (hex(), hex(), hex());
            assert!(fields.next().is_none());
            Vector {
                base,
                exponent,
                want,
            }
        })
        .collect()
}

fn check(name: &str, vectors: &[Vector], eval: impl Fn(f64, f64) -> f64) {
    let mut failures = Vec::new();
    for v in vectors {
        let got = eval(f64::from_bits(v.base), f64::from_bits(v.exponent)).to_bits();
        if got != v.want {
            failures.push(format!(
                "pow {:016x} {:016x}: got {got:016x}, want {:016x}",
                v.base, v.exponent, v.want
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{name}: {} of {} vectors differ, first {:#?}",
        failures.len(),
        vectors.len(),
        &failures[..failures.len().min(5)]
    );
}

#[cfg(not(target_arch = "aarch64"))]
const DEFAULT_NAN: u64 = 0xfff8_0000_0000_0000;
#[cfg(not(target_arch = "aarch64"))]
const ARM64_DEFAULT_NAN: u64 = 0x7ff8_0000_0000_0000;

/// The square root of a negative number is the processor's default NaN, whose sign
/// differs between x86 and arm64; on a host that is not arm64 the arm64 vectors expect
/// the arm64 one.
fn arm64_pow(x: f64, y: f64) -> f64 {
    let got = pow(x, y, arm64::exp, go::log::<true>);
    #[cfg(not(target_arch = "aarch64"))]
    if got.to_bits() == DEFAULT_NAN {
        return f64::from_bits(ARM64_DEFAULT_NAN);
    }
    got
}

#[test]
fn arm64_path_matches_go() {
    let vectors = parse(ARM64);
    assert!(vectors.len() > 50_000);
    check("arm64", &vectors, arm64_pow);
}

#[cfg(target_arch = "x86_64")]
mod amd64_vectors {
    use super::{check, parse, pow};
    use crate::math::amd64;

    const AMD64_FMA: &str = include_str!("../../../testdata/pow-amd64-fma.txt");
    const AMD64_NOFMA: &str = include_str!("../../../testdata/pow-amd64-nofma.txt");

    #[test]
    fn fma_branch_matches_go() {
        let vectors = parse(AMD64_FMA);
        assert!(vectors.len() > 50_000);
        check("amd64 fma", &vectors, |x, y| {
            pow(x, y, amd64::exp_fma, amd64::log)
        });
    }

    #[test]
    fn nofma_branch_matches_go() {
        let vectors = parse(AMD64_NOFMA);
        assert!(vectors.len() > 10_000);
        check("amd64 nofma", &vectors, |x, y| {
            pow(x, y, amd64::exp_plain, amd64::log)
        });
    }

    #[test]
    fn the_two_branches_differ() {
        let differing = parse(AMD64_NOFMA)
            .iter()
            .filter(|v| {
                pow(
                    f64::from_bits(v.base),
                    f64::from_bits(v.exponent),
                    amd64::exp_fma,
                    amd64::log,
                )
                .to_bits()
                    != v.want
            })
            .count();
        assert!(differing > 100, "{differing}");
    }

    #[test]
    fn dispatch_follows_the_cpu() {
        let vectors = if amd64::use_fma() {
            parse(AMD64_FMA)
        } else {
            parse(AMD64_NOFMA)
        };
        check("dispatch", &vectors, crate::math::pow);
    }
}

#[cfg(target_arch = "aarch64")]
#[test]
fn dispatch_is_the_arm64_path() {
    check("arm64", &parse(ARM64), crate::math::pow);
}

#[test]
fn frexp_and_ldexp_invert_each_other() {
    for x in [
        1.0,
        -1.0,
        0.75,
        1e300,
        -1e-300,
        5e-324,
        f64::MIN_POSITIVE,
        f64::MAX,
    ] {
        let (frac, exp) = frexp(x);
        assert!((0.5..1.0).contains(&frac.abs()), "{x}");
        assert_eq!(ldexp(frac, exp).to_bits(), x.to_bits(), "{x}");
    }
    assert_eq!(frexp(8.0), (0.5, 4));
    assert_eq!(frexp(-0.0).0.to_bits(), (-0.0_f64).to_bits());
    assert_eq!(frexp(f64::INFINITY), (f64::INFINITY, 0));
    assert!(frexp(f64::NAN).0.is_nan());
}

#[test]
fn ldexp_saturates() {
    assert_eq!(ldexp(1.0, 1024), f64::INFINITY);
    assert_eq!(ldexp(-1.0, 1024), f64::NEG_INFINITY);
    assert_eq!(ldexp(1.0, -1076).to_bits(), 0);
    assert_eq!(ldexp(-1.0, -1076).to_bits(), (-0.0_f64).to_bits());
    assert_eq!(ldexp(1.0, -1074).to_bits(), 1);
    assert_eq!(ldexp(1.0, 1023), 2.0_f64.powi(1023));
}

#[test]
fn modf_keeps_the_sign() {
    assert_eq!(modf(-2.5), (-2.0, -0.5));
    assert_eq!(modf(2.5), (2.0, 0.5));
    let (i, f) = modf(-0.25);
    assert_eq!(i.to_bits(), (-0.0_f64).to_bits());
    assert_eq!(f, -0.25);
}

#[test]
fn odd_integers() {
    assert!(is_odd_int(3.0));
    assert!(is_odd_int(-3.0));
    assert!(!is_odd_int(2.0));
    assert!(!is_odd_int(2.5));
    assert!(is_odd_int(9_007_199_254_740_991.0));
    assert!(!is_odd_int(9_007_199_254_740_992.0));
    assert!(!is_odd_int(1e19));
    assert!(!is_odd_int(f64::INFINITY));
    assert!(!is_odd_int(f64::NAN));
}
