use std::cell::Cell;

use super::tables::{FE, KE, WE};
use super::{RE, Uint64Source, exp_float64};
use crate::math::{arm64, go};

const ARM64: &str = include_str!("../../../testdata/expfloat-arm64.txt");

struct Vector {
    draws: Vec<u64>,
    want: u64,
}

fn parse(text: &'static str) -> Vec<Vector> {
    assert!(text.ends_with('\n'));
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split(' ');
            assert_eq!(fields.next(), Some("expfloat"));
            let n: usize = fields.next().unwrap().parse().unwrap();
            let mut hex = || u64::from_str_radix(fields.next().unwrap(), 16).unwrap();
            let draws = (0..n).map(|_| hex()).collect();
            let want = hex();
            assert!(fields.next().is_none());
            Vector { draws, want }
        })
        .collect()
}

struct Script<'a> {
    draws: &'a [u64],
    used: usize,
}

impl Uint64Source for Script<'_> {
    fn uint64(&mut self) -> u64 {
        let u = self.draws.get(self.used).copied().unwrap_or(0);
        self.used += 1;
        u
    }
}

fn check(name: &str, vectors: &[Vector], call: impl Fn(&mut Script) -> f64) {
    let mut failures = Vec::new();
    for v in vectors {
        let mut script = Script {
            draws: &v.draws,
            used: 0,
        };
        let got = call(&mut script).to_bits();
        if got != v.want || script.used != v.draws.len() {
            failures.push(format!(
                "draws {:016x?}: got {got:016x} after {} draws, want {:016x} after {}",
                v.draws,
                script.used,
                v.want,
                v.draws.len()
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

fn arm64_call(src: &mut Script) -> f64 {
    exp_float64::<true>(src, arm64::exp, go::log::<true>)
}

#[test]
fn arm64_path_matches_go() {
    let vectors = parse(ARM64);
    assert!(vectors.len() > 20_000);
    check("arm64", &vectors, arm64_call);
}

#[test]
fn the_arm64_float32_fusion_matters() {
    let differing = parse(ARM64)
        .iter()
        .filter(|v| {
            let mut script = Script {
                draws: &v.draws,
                used: 0,
            };
            exp_float64::<false>(&mut script, arm64::exp, go::log::<true>).to_bits() != v.want
                || script.used != v.draws.len()
        })
        .count();
    assert!(differing > 500, "{differing}");
}

#[cfg(target_arch = "x86_64")]
mod amd64_vectors {
    use super::{Vector, check, exp_float64, parse};
    use crate::math::amd64;

    const AMD64_FMA: &str = include_str!("../../../testdata/expfloat-amd64-fma.txt");
    const AMD64_NOFMA: &str = include_str!("../../../testdata/expfloat-amd64-nofma.txt");

    #[test]
    fn fma_branch_matches_go() {
        let vectors = parse(AMD64_FMA);
        assert!(vectors.len() > 20_000);
        check("amd64 fma", &vectors, |src| {
            exp_float64::<false>(src, amd64::exp_fma, amd64::log)
        });
    }

    #[test]
    fn nofma_branch_matches_go() {
        let vectors = parse(AMD64_NOFMA);
        assert!(vectors.len() > 10_000);
        check("amd64 nofma", &vectors, |src| {
            exp_float64::<false>(src, amd64::exp_plain, amd64::log)
        });
    }

    #[test]
    fn dispatch_follows_the_cpu() {
        let vectors: Vec<Vector> = if amd64::use_fma() {
            parse(AMD64_FMA)
        } else {
            parse(AMD64_NOFMA)
        };
        check("dispatch", &vectors, |src| crate::math::exp_float64(src));
    }
}

#[cfg(target_arch = "aarch64")]
#[test]
fn dispatch_is_the_arm64_path() {
    check("arm64", &parse(ARM64), |src| crate::math::exp_float64(src));
}

fn first_draw(v: &Vector) -> (u32, usize) {
    (v.draws[0] as u32, usize::from((v.draws[0] >> 32) as u8))
}

#[test]
fn forced_vectors_reach_the_tail_and_the_rejection_path() {
    let vectors = parse(ARM64);
    let (mut fast, mut tail, mut reject_accepted, mut reject_redrawn) = (0, 0, 0, 0);
    let mut chained = 0;
    for v in &vectors {
        let (j, i) = first_draw(v);
        let logs = Cell::new(0);
        let exps = Cell::new(0);
        let mut script = Script {
            draws: &v.draws,
            used: 0,
        };
        exp_float64::<true>(
            &mut script,
            |x| {
                exps.set(exps.get() + 1);
                arm64::exp(x)
            },
            |x| {
                logs.set(logs.get() + 1);
                go::log::<true>(x)
            },
        );
        if j < KE[i] {
            assert_eq!((logs.get(), exps.get(), v.draws.len()), (0, 0, 1));
            fast += 1;
        } else if i == 0 {
            assert_eq!((logs.get(), exps.get(), v.draws.len()), (1, 0, 2));
            tail += 1;
        } else if v.draws.len() == 2 {
            assert_eq!((logs.get(), exps.get()), (0, 1));
            reject_accepted += 1;
        } else {
            assert!(exps.get() >= 1);
            reject_redrawn += 1;
        }
        if v.draws.len() > 4 {
            chained += 1;
        }
    }
    assert!(fast > 5_000, "{fast}");
    assert!(tail > 500, "{tail}");
    assert!(reject_accepted > 1_000, "{reject_accepted}");
    assert!(reject_redrawn > 1_000, "{reject_redrawn}");
    assert!(chained > 50, "{chained}");
}

#[test]
fn every_table_row_is_forced_on_the_rejection_path() {
    let mut rows = [false; 256];
    for v in &parse(ARM64) {
        let (j, i) = first_draw(v);
        if i != 0 && j >= KE[i] {
            rows[i] = true;
        }
    }
    assert!(rows[1..].iter().all(|&seen| seen));
}

#[test]
fn the_tail_with_a_zero_float_is_infinite() {
    let mut script = Script {
        draws: &[u64::from(u32::MAX), 0],
        used: 0,
    };
    assert_eq!(arm64_call(&mut script), f64::INFINITY);
}

#[test]
fn float_draws_use_the_low_53_bits() {
    let low = 0x0012_3456_789a_bcde_u64;
    let a = [u64::from(u32::MAX), low];
    let b = [u64::from(u32::MAX), low | (0x7ff << 53)];
    let call = |draws: &[u64]| arm64_call(&mut Script { draws, used: 0 }).to_bits();
    assert_eq!(call(&a), call(&b));
}

fn near_equal(a: f64, b: f64, close_enough: f64, max_error: f64) -> bool {
    let diff = (a - b).abs();
    diff < close_enough || diff / a.abs().max(b.abs()) < max_error
}

#[expect(
    clippy::cast_sign_loss,
    reason = "Go's initExp converts the positive products the same way"
)]
#[test]
fn tables_match_init_exp() {
    const M2: f64 = (1u64 << 32) as f64;
    let exp = crate::math::exp;
    let log = crate::math::log;
    let mut de = RE;
    let mut te = de;
    let ve = 3.949_659_822_581_557e-3;
    let mut ke = [0u32; 256];
    let mut we = [0f32; 256];
    let mut fe = [0f32; 256];
    let q = ve / exp(-de);
    ke[0] = ((de / q) * M2) as u32;
    ke[1] = 0;
    we[0] = (q / M2) as f32;
    we[255] = (de / M2) as f32;
    fe[0] = 1.0;
    fe[255] = exp(-de) as f32;
    for i in (1..=254).rev() {
        de = -log(ve / de + exp(-de));
        ke[i + 1] = ((de / te) * M2) as u32;
        te = de;
        fe[i] = exp(-de) as f32;
        we[i] = (de / M2) as f32;
    }
    for i in 0..256 {
        assert_eq!(KE[i], ke[i], "ke[{i}]");
        assert!(
            near_equal(
                f64::from(f32::from_bits(WE[i])),
                f64::from(we[i]),
                0.0,
                1e-7
            ),
            "we[{i}]"
        );
        assert!(
            near_equal(
                f64::from(f32::from_bits(FE[i])),
                f64::from(fe[i]),
                0.0,
                1e-7
            ),
            "fe[{i}]"
        );
    }
}
