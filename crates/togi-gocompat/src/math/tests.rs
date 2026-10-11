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

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{arm64, go};

    #[target_feature(enable = "fma")]
    pub(super) fn arm64_exp(x: f64) -> f64 {
        arm64::exp(x)
    }

    #[target_feature(enable = "fma")]
    pub(super) fn arm64_expm1(x: f64) -> f64 {
        go::expm1::<true>(x)
    }

    #[target_feature(enable = "fma")]
    pub(super) fn arm64_log(x: f64) -> f64 {
        go::log::<true>(x)
    }

    #[target_feature(enable = "fma")]
    pub(super) fn arm64_log1p(x: f64) -> f64 {
        go::log1p::<true>(x)
    }
}

#[cfg(target_arch = "x86_64")]
#[expect(
    unsafe_code,
    reason = "calls FMA-compiled code after the feature check"
)]
fn arm64_eval(func: &str, x: f64) -> f64 {
    assert!(
        is_x86_feature_detected!("fma"),
        "running the arm64 port on x86_64 needs FMA"
    );
    // SAFETY: the CPU supports FMA, checked above.
    unsafe {
        match func {
            "exp" => x86::arm64_exp(x),
            "expm1" => x86::arm64_expm1(x),
            "log" => x86::arm64_log(x),
            "log1p" => x86::arm64_log1p(x),
            other => unknown(other),
        }
    }
}

#[cfg(target_arch = "aarch64")]
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

    #[expect(
        unsafe_code,
        reason = "calls FMA-compiled code after the feature check"
    )]
    fn fma_exp(x: f64) -> f64 {
        assert!(
            amd64::use_fma(),
            "the FMA branch of archExp needs a CPU with AVX and FMA"
        );
        // SAFETY: the CPU supports FMA, checked above.
        unsafe { amd64::exp_fma(x) }
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
