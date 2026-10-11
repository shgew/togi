use super::{go, lgamma::lgamma};

const ARM64: &str = include_str!("../../testdata/lgamma-arm64.txt");
#[cfg(target_arch = "x86_64")]
const AMD64: &str = include_str!("../../testdata/lgamma-amd64.txt");

struct Vector {
    input: u64,
    want: u64,
    sign: i32,
}

fn parse(text: &'static str) -> Vec<Vector> {
    assert!(text.ends_with('\n'));
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split(' ');
            assert_eq!(fields.next(), Some("lgamma"));
            let input = u64::from_str_radix(fields.next().unwrap(), 16).unwrap();
            let want = u64::from_str_radix(fields.next().unwrap(), 16).unwrap();
            let sign = fields.next().unwrap().parse().unwrap();
            assert!(fields.next().is_none());
            Vector { input, want, sign }
        })
        .collect()
}

fn check(name: &str, vectors: &[Vector], eval: impl Fn(f64) -> (f64, i32)) {
    assert!(vectors.len() > 40_000, "{name}: {}", vectors.len());
    let mut failures = Vec::new();
    for v in vectors {
        let (got, sign) = eval(f64::from_bits(v.input));
        if got.to_bits() != v.want || sign != v.sign {
            failures.push(format!(
                "{:016x}: got {:016x} {sign}, want {:016x} {}",
                v.input,
                got.to_bits(),
                v.want,
                v.sign
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

fn arm64_eval(x: f64) -> (f64, i32) {
    lgamma::<true>(x, go::log::<true>)
}

#[test]
fn arm64_path_matches_go() {
    check("arm64", &parse(ARM64), arm64_eval);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn amd64_path_matches_go() {
    check("amd64", &parse(AMD64), |x| {
        lgamma::<false>(x, crate::math::amd64::log)
    });
}

#[test]
fn dispatch_follows_the_target() {
    #[cfg(target_arch = "x86_64")]
    check("amd64", &parse(AMD64), super::lgamma);
    #[cfg(target_arch = "aarch64")]
    check("arm64", &parse(ARM64), super::lgamma);
}

#[test]
fn the_targets_differ() {
    let differing = parse(ARM64)
        .iter()
        .filter(|v| {
            lgamma::<false>(f64::from_bits(v.input), go::log::<false>)
                .0
                .to_bits()
                != v.want
        })
        .count();
    assert!(differing > 100, "{differing}");
}

/// `vf` from Go's `src/math/all_test.go`, lines 14-25.
#[expect(
    clippy::excessive_precision,
    reason = "the literals are Go's own, copied verbatim"
)]
const GO_VF: [f64; 10] = [
    4.979_011_924_883_673_5e+00,
    7.738_872_474_578_104_5e+00,
    -2.768_800_571_920_015_9e-01,
    -5.010_603_618_271_074_9e+00,
    9.636_293_707_198_417_3e+00,
    2.926_377_239_243_964_6e+00,
    5.229_083_431_459_306_6e+00,
    2.727_939_910_436_010_2e+00,
    1.825_308_091_680_855e+00,
    -8.685_924_768_575_601_3e+00,
];

/// Go's `lgamma` table (`all_test.go` lines 401-412), compared by `close`.
#[expect(
    clippy::excessive_precision,
    reason = "the literals are Go's own, copied verbatim"
)]
const GO_LGAMMA: [(f64, i32); 10] = [
    (3.146_492_141_244_545_8e+00, 1),
    (8.003_414_490_659_126e+00, 1),
    (1.517_575_735_509_779_7e+00, -1),
    (-2.588_480_028_182_145_8e-01, 1),
    (1.198_989_705_020_555_5e+01, 1),
    (6.262_899_811_091_257_5e-01, 1),
    (3.528_792_489_909_156_6e+00, 1),
    (4.572_564_477_016_118_2e-01, 1),
    (-6.363_667_087_767_961e-02, 1),
    (-1.077_385_130_910_300_1e+01, -1),
];

/// Go's `vflgammaSC` and `lgammaSC` (lines 1518-1535).
fn go_sc() -> [(f64, f64, i32); 7] {
    let nan = f64::from_bits(0x7ff8_0000_0000_0001);
    [
        (f64::NEG_INFINITY, f64::NEG_INFINITY, 1),
        (-3.0, f64::INFINITY, 1),
        (0.0, f64::INFINITY, 1),
        (1.0, 0.0, 1),
        (2.0, 0.0, 1),
        (f64::INFINITY, f64::INFINITY, 1),
        (nan, nan, 1),
    ]
}

fn close(a: f64, b: f64) -> bool {
    ((a - b) / a).abs() < 1e-14
}

fn go_test_lgamma(name: &str, vectors: &[Vector], eval: impl Fn(f64) -> (f64, i32)) {
    let find = |x: f64| {
        vectors
            .iter()
            .find(|v| v.input == x.to_bits())
            .unwrap_or_else(|| panic!("{name}: {x} is not in the vectors"))
    };
    for (x, (want, want_sign)) in GO_VF.iter().zip(GO_LGAMMA) {
        let (got, sign) = eval(*x);
        assert!(close(want, got) && sign == want_sign, "{name}: {x}");
        let v = find(*x);
        assert_eq!((got.to_bits(), sign), (v.want, v.sign), "{name}: {x}");
    }
    for (x, want, want_sign) in go_sc() {
        let (got, sign) = eval(x);
        let alike = if want.is_nan() {
            got.is_nan()
        } else {
            want.to_bits() == got.to_bits()
        };
        assert!(alike && sign == want_sign, "{name}: {x}");
        let v = find(x);
        assert_eq!((got.to_bits(), sign), (v.want, v.sign), "{name}: {x}");
    }
}

#[test]
fn go_test_lgamma_arm64() {
    go_test_lgamma("arm64", &parse(ARM64), arm64_eval);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn go_test_lgamma_amd64() {
    go_test_lgamma("amd64", &parse(AMD64), super::lgamma);
}
