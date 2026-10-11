use super::{arm64, go};

const ARM64: &str = include_str!("../../testdata/math-arm64.txt");

struct Vector {
    func: &'static str,
    input: u64,
    want: u64,
}

fn parse(text: &'static str) -> Vec<Vector> {
    assert!(text.ends_with('\n'));
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split(' ');
            let func = fields.next().unwrap();
            let input = u64::from_str_radix(fields.next().unwrap(), 16).unwrap();
            let want = u64::from_str_radix(fields.next().unwrap(), 16).unwrap();
            assert!(fields.next().is_none());
            Vector { func, input, want }
        })
        .collect()
}

fn check(name: &str, vectors: &[Vector], mut eval: impl FnMut(&str, f64) -> f64) {
    let mut failures = Vec::new();
    for v in vectors {
        let got = eval(v.func, f64::from_bits(v.input)).to_bits();
        if got != v.want {
            failures.push(format!(
                "{} {:016x}: got {got:016x}, want {:016x}",
                v.func, v.input, v.want
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

fn count(vectors: &[Vector], func: &str) -> usize {
    vectors.iter().filter(|v| v.func == func).count()
}

fn unknown(func: &str) -> ! {
    panic!("unknown function {func}")
}

fn arm64_eval(func: &str, x: f64) -> f64 {
    match func {
        "exp" => arm64::exp(x),
        "expm1" => go::expm1::<true>(x),
        "log" => go::log::<true>(x),
        "log1p" => go::log1p::<true>(x),
        other => unknown(other),
    }
}

#[test]
fn arm64_path_matches_go() {
    let vectors = parse(ARM64);
    for func in ["exp", "expm1", "log", "log1p"] {
        assert!(count(&vectors, func) > 10_000, "{func}");
    }
    check("arm64", &vectors, arm64_eval);
}

#[cfg(target_arch = "x86_64")]
mod amd64_vectors {
    use super::{check, count, go, parse, unknown};
    use crate::math::amd64;

    const AMD64_FMA: &str = include_str!("../../testdata/math-amd64-fma.txt");
    const AMD64_NOFMA: &str = include_str!("../../testdata/math-amd64-nofma.txt");

    fn fma_exp(x: f64) -> f64 {
        amd64::exp_fma(x)
    }

    #[test]
    fn fma_branch_matches_go() {
        let vectors = parse(AMD64_FMA);
        for func in ["exp", "expm1", "log", "log1p"] {
            assert!(count(&vectors, func) > 10_000, "{func}");
        }
        check("amd64 fma", &vectors, |func, x| match func {
            "exp" => fma_exp(x),
            "expm1" => go::expm1::<false>(x),
            "log" => amd64::log(x),
            "log1p" => go::log1p::<false>(x),
            other => unknown(other),
        });
    }

    #[test]
    fn nofma_branch_matches_go() {
        let vectors = parse(AMD64_NOFMA);
        assert!(count(&vectors, "exp") > 10_000);
        assert_eq!(count(&vectors, "exp"), vectors.len());
        check("amd64 nofma", &vectors, |_, x| amd64::exp_plain(x));
    }

    #[test]
    fn the_two_branches_differ() {
        let vectors = parse(AMD64_FMA);
        let differing = vectors
            .iter()
            .filter(|v| v.func == "exp")
            .filter(|v| amd64::exp_plain(f64::from_bits(v.input)).to_bits() != v.want)
            .count();
        assert!(differing > 1_000, "{differing}");
    }

    #[test]
    fn dispatch_follows_the_cpu() {
        let (vectors, branch) = if amd64::use_fma() {
            (parse(AMD64_FMA), "fma")
        } else {
            (parse(AMD64_NOFMA), "nofma")
        };
        check(branch, &vectors, |func, x| match func {
            "exp" => crate::math::exp(x),
            "expm1" => crate::math::expm1(x),
            "log" => crate::math::log(x),
            "log1p" => crate::math::log1p(x),
            other => unknown(other),
        });
    }
}

#[cfg(target_arch = "aarch64")]
#[test]
fn dispatch_is_the_arm64_path() {
    check("arm64", &parse(ARM64), |func, x| match func {
        "exp" => super::exp(x),
        "expm1" => super::expm1(x),
        "log" => super::log(x),
        "log1p" => super::log1p(x),
        other => unknown(other),
    });
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

/// Go's `NaN()`, whose bits differ from `f64::NAN`.
fn go_nan() -> f64 {
    f64::from_bits(0x7ff8_0000_0000_0001)
}

/// The inputs of Go's `TestExp` (`all_test.go` lines 2438-2454): `vf[i]` and `vfexpSC`
/// (lines 1058-1072). Go's `testExp` also runs the pure-Go `ExpGo`, which no path of
/// this crate runs.
fn go_test_exp_inputs() -> Vec<f64> {
    let mut xs = GO_VF.to_vec();
    xs.extend([
        f64::NEG_INFINITY,
        -2000.0,
        2000.0,
        f64::INFINITY,
        go_nan(),
        7.097_827_128_933_841e+02,
        1.488_522_23e+09,
        1.488_522_2e+09,
        1.0,
        3.725_290_298_461_915e-09,
        -740.0,
    ]);
    xs
}

/// The inputs of Go's `TestExpm1` (lines 2456-2474): `vf[i]/100`, `vf[i]*10` and
/// `vfexpm1SC` (lines 1113-1121).
fn go_test_expm1_inputs() -> Vec<f64> {
    let mut xs: Vec<f64> = GO_VF.iter().map(|v| v / 100.0).collect();
    xs.extend(GO_VF.iter().map(|v| v * 10.0));
    xs.extend([
        f64::NEG_INFINITY,
        -710.0,
        -0.0,
        0.0,
        710.0,
        f64::INFINITY,
        go_nan(),
    ]);
    xs
}

/// The inputs of Go's `TestLog` (lines 2771-2786): `Abs(vf[i])`, `10` and `vflogSC`
/// (lines 1537-1545).
fn go_test_log_inputs() -> Vec<f64> {
    let mut xs: Vec<f64> = GO_VF.iter().map(|v| v.abs()).collect();
    xs.push(10.0);
    xs.extend([
        f64::NEG_INFINITY,
        -std::f64::consts::PI,
        -0.0,
        0.0,
        1.0,
        f64::INFINITY,
        go_nan(),
    ]);
    xs
}

/// The inputs of Go's `TestLog1p` (lines 2823-2839): `vf[i]/100`, `9` and the first
/// `len(vflogSC)` = 7 entries of `vflog1pSC` (lines 1569-1578), which is all Go iterates.
fn go_test_log1p_inputs() -> Vec<f64> {
    let mut xs: Vec<f64> = GO_VF.iter().map(|v| v / 100.0).collect();
    xs.push(9.0);
    xs.extend([
        f64::NEG_INFINITY,
        -std::f64::consts::PI,
        -1.0,
        -0.0,
        0.0,
        f64::INFINITY,
        go_nan(),
    ]);
    xs
}

/// Requires every input to be in the vectors for `func` and `eval` to return the
/// vector's bits for it.
fn check_go_test(
    name: &str,
    vectors: &[Vector],
    func: &str,
    inputs: &[f64],
    eval: impl Fn(f64) -> f64,
) {
    let mut failures = Vec::new();
    for &x in inputs {
        let bits = x.to_bits();
        match vectors.iter().find(|v| v.func == func && v.input == bits) {
            None => failures.push(format!("{func} {bits:016x}: not in the vectors")),
            Some(v) => {
                let got = eval(x).to_bits();
                if got != v.want {
                    failures.push(format!(
                        "{func} {bits:016x}: got {got:016x}, want {:016x}",
                        v.want
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{name}: {failures:#?}");
}

#[test]
fn go_test_exp() {
    let inputs = go_test_exp_inputs();
    check_go_test("arm64", &parse(ARM64), "exp", &inputs, arm64::exp);
    #[cfg(target_arch = "x86_64")]
    {
        use crate::math::amd64;
        check_go_test(
            "amd64 fma",
            &parse(include_str!("../../testdata/math-amd64-fma.txt")),
            "exp",
            &inputs,
            amd64::exp_fma,
        );
        check_go_test(
            "amd64 nofma",
            &parse(include_str!("../../testdata/math-amd64-nofma.txt")),
            "exp",
            &inputs,
            amd64::exp_plain,
        );
    }
}

#[test]
fn go_test_expm1() {
    let inputs = go_test_expm1_inputs();
    check_go_test("arm64", &parse(ARM64), "expm1", &inputs, go::expm1::<true>);
    #[cfg(target_arch = "x86_64")]
    check_go_test(
        "amd64",
        &parse(include_str!("../../testdata/math-amd64-fma.txt")),
        "expm1",
        &inputs,
        go::expm1::<false>,
    );
}

#[test]
fn go_test_log() {
    let inputs = go_test_log_inputs();
    check_go_test("arm64", &parse(ARM64), "log", &inputs, go::log::<true>);
    assert_eq!(
        go::log::<true>(10.0).to_bits(),
        std::f64::consts::LN_10.to_bits()
    );
    #[cfg(target_arch = "x86_64")]
    {
        use crate::math::amd64;
        check_go_test(
            "amd64",
            &parse(include_str!("../../testdata/math-amd64-fma.txt")),
            "log",
            &inputs,
            amd64::log,
        );
        assert_eq!(
            amd64::log(10.0).to_bits(),
            std::f64::consts::LN_10.to_bits()
        );
    }
}

#[test]
fn go_test_log1p() {
    let inputs = go_test_log1p_inputs();
    check_go_test("arm64", &parse(ARM64), "log1p", &inputs, go::log1p::<true>);
    assert_eq!(
        go::log1p::<true>(9.0).to_bits(),
        std::f64::consts::LN_10.to_bits()
    );
    #[cfg(target_arch = "x86_64")]
    {
        check_go_test(
            "amd64",
            &parse(include_str!("../../testdata/math-amd64-fma.txt")),
            "log1p",
            &inputs,
            go::log1p::<false>,
        );
        assert_eq!(
            go::log1p::<false>(9.0).to_bits(),
            std::f64::consts::LN_10.to_bits()
        );
    }
}
