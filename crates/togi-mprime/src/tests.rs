use super::*;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use tempfile::TempDir;
use togi_machine::{REGIMES, workload_by_id, workloads};

const TESTDATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata");

fn fake_package() -> TempDir {
    let pkg = TempDir::new().unwrap();
    fs::create_dir(pkg.path().join("bin")).unwrap();
    let binary = pkg.path().join("bin/mprime");
    fs::write(&binary, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    pkg
}

fn cpus(ids: &[u16]) -> Vec<CpuId> {
    ids.iter().map(|&id| CpuId(id)).collect()
}

fn workload(base: &str) -> &'static Workload {
    workload_by_id(&format!("{base}-smt")).unwrap()
}

fn names(plan: &Plan) -> Vec<&str> {
    plan.inputs
        .iter()
        .map(|input| input.name.as_str())
        .collect()
}

fn text(plan: &Plan, name: &str) -> String {
    let input = plan
        .inputs
        .iter()
        .find(|input| input.name.as_str() == name)
        .unwrap();
    String::from_utf8(input.contents.clone()).unwrap()
}

#[test]
fn identity_and_detail() {
    let pkg = fake_package();
    let mprime = Mprime::new(pkg.path()).unwrap();
    assert_eq!(mprime.kind(), togi_machine::Backend::Mprime);
    assert_eq!(
        mprime.check().unwrap().detail,
        pkg.path().join("bin/mprime").display().to_string()
    );
}

#[test]
fn missing_binary_is_backend_missing() {
    let pkg = fake_package();
    fs::remove_file(pkg.path().join("bin/mprime")).unwrap();
    let error = Mprime::new(pkg.path()).unwrap().check().unwrap_err();
    assert!(error.is_missing());
    assert!(matches!(
        togi_machine::Error::from(error),
        togi_machine::Error::BackendMissing
    ));
}

#[test]
fn non_executable_binary_fails_check() {
    let pkg = fake_package();
    fs::set_permissions(
        pkg.path().join("bin/mprime"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let error = Mprime::new(pkg.path()).unwrap().check().unwrap_err();
    assert!(matches!(error, Error::NotExecutable { .. }));
    assert!(!error.is_missing());
}

#[test]
fn stat_failure_is_not_backend_missing() {
    let pkg = fake_package();
    let binary = pkg.path().join("bin/mprime");
    fs::remove_file(&binary).unwrap();
    symlink("mprime", &binary).unwrap();
    let error = Mprime::new(pkg.path()).unwrap().check().unwrap_err();
    assert!(!error.is_missing());
    assert!(error.to_string().contains(&binary.display().to_string()));
}

fn expected_settings(fft: (u16, u16), flags: [u8; 4], hyper: u8, threads: usize) -> String {
    format!(
        "NumCPUs=1\nCoresPerTest=1\nMinTortureFFT={}\nMaxTortureFFT={}\nTortureTime=1\n\
         TortureHyperthreading={hyper}\nTortureThreads={threads}\nCpuSupportsAVX={}\n\
         CpuSupportsFMA3={}\nCpuSupportsFMA4=0\nCpuSupportsAVX2={}\nCpuSupportsAVX512F={}\n",
        fft.0, fft.1, flags[0], flags[1], flags[2], flags[3]
    )
}

#[test]
fn plans_match_exactly() {
    let pkg = fake_package();
    let mprime = Mprime::new(pkg.path()).unwrap();
    let dir = TempDir::new().unwrap();
    for (base, fft, flags, ids, hyper) in [
        ("mprime-sse-4k-21k", (4, 21), [0, 0, 0, 0], vec![2], 0),
        (
            "mprime-avx512-36k-248k",
            (36, 248),
            [1, 1, 1, 1],
            vec![2, 18],
            1,
        ),
        ("mprime-avx2-36k-248k", (36, 248), [1, 1, 1, 0], vec![2], 0),
    ] {
        let got = mprime
            .plan(workload(base), dir.path(), &cpus(&ids))
            .unwrap();
        let settings = expected_settings(fft, flags, hyper, ids.len());
        let want = Plan {
            program: pkg.path().join("bin/mprime"),
            args: vec![
                OsString::from("-t"),
                OsString::from(format!("-W{}", dir.path().display())),
            ],
            inputs: vec![
                Input {
                    name: "prime.txt".parse().unwrap(),
                    contents: format!(
                        "V30OptionsConverted=1\nStressTester=1\nUsePrimenet=0\n{settings}EnableSetAffinity=0\n"
                    )
                    .into_bytes(),
                },
                Input {
                    name: "local.txt".parse().unwrap(),
                    contents: format!("ErrorCheck=1\nSumInputsErrorCheck=1\n{settings}")
                        .into_bytes(),
                },
            ],
            watched: vec!["results.txt".parse().unwrap()],
        };
        assert_eq!(got, want, "{base}");
    }
}

#[test]
fn sse_single_cpu_local_txt_is_literal() {
    let pkg = fake_package();
    let plan = Mprime::new(pkg.path())
        .unwrap()
        .plan(workload("mprime-sse-4k-21k"), Path::new("/w"), &cpus(&[2]))
        .unwrap();
    assert_eq!(
        text(&plan, "local.txt"),
        "ErrorCheck=1\nSumInputsErrorCheck=1\nNumCPUs=1\nCoresPerTest=1\nMinTortureFFT=4\nMaxTortureFFT=21\nTortureTime=1\nTortureHyperthreading=0\nTortureThreads=1\nCpuSupportsAVX=0\nCpuSupportsFMA3=0\nCpuSupportsFMA4=0\nCpuSupportsAVX2=0\nCpuSupportsAVX512F=0\n"
    );
    assert_eq!(plan.args[1], OsString::from("-W/w"));
}

#[test]
fn thread_count_and_hyperthreading_follow_the_cpu_list() {
    let pkg = fake_package();
    let mprime = Mprime::new(pkg.path()).unwrap();
    for (ids, hyper) in [(vec![2], 0), (vec![2, 18], 1), (vec![2, 18, 3, 19], 1)] {
        let plan = mprime
            .plan(workload("mprime-sse-4k-21k"), Path::new("/w"), &cpus(&ids))
            .unwrap();
        for name in ["prime.txt", "local.txt"] {
            let contents = text(&plan, name);
            assert!(contents.contains(&format!("TortureThreads={}\n", ids.len())));
            assert!(contents.contains(&format!("TortureHyperthreading={hyper}\n")));
        }
    }
}

#[test]
fn unsupported_workload_is_refused_before_the_binary_is_checked() {
    let pkg = TempDir::new().unwrap();
    let error = Mprime::new(pkg.path())
        .unwrap()
        .plan(
            workload("ycruncher-bkt-sftv4"),
            Path::new("/w"),
            &cpus(&[2]),
        )
        .unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }));
    assert!(!error.is_missing());
    assert!(error.to_string().contains("ycruncher-bkt-sftv4-smt"));
}

#[test]
fn a_plan_needs_a_cpu() {
    let pkg = fake_package();
    let error = Mprime::new(pkg.path())
        .unwrap()
        .plan(workload("mprime-sse-4k-21k"), Path::new("/w"), &[])
        .unwrap_err();
    assert!(matches!(error, Error::NoCpus { .. }));
}

#[test]
fn missing_binary_yields_no_plan() {
    let pkg = fake_package();
    fs::remove_file(pkg.path().join("bin/mprime")).unwrap();
    let dir = TempDir::new().unwrap();
    let error = Mprime::new(pkg.path())
        .unwrap()
        .plan(workload("mprime-sse-4k-21k"), dir.path(), &cpus(&[2]))
        .unwrap_err();
    assert!(error.is_missing());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn planning_writes_nothing() {
    let pkg = fake_package();
    let dir = TempDir::new().unwrap();
    Mprime::new(pkg.path())
        .unwrap()
        .plan(workload("mprime-sse-4k-21k"), dir.path(), &cpus(&[2]))
        .unwrap();
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn every_catalog_workload_plans() {
    let pkg = fake_package();
    let mprime = Mprime::new(pkg.path()).unwrap();
    let dir = TempDir::new().unwrap();
    let mut planned = BTreeSet::new();
    for regime in REGIMES {
        for workload in workloads(regime) {
            if workload.backend != togi_machine::Backend::Mprime {
                continue;
            }
            let ids = [2, 18];
            let plan = mprime
                .plan(
                    workload,
                    dir.path(),
                    &cpus(&ids[..usize::from(workload.threads)]),
                )
                .unwrap_or_else(|error| panic!("{regime}/{}: {error}", workload.id));
            assert!(plan.program.is_absolute());
            assert!(plan.program.starts_with(pkg.path()));
            assert!(fs::metadata(&plan.program).unwrap().permissions().mode() & 0o111 != 0);
            assert_eq!(names(&plan), ["prime.txt", "local.txt"]);
            assert_eq!(plan.watched.len(), 1);
            planned.insert(workload.base);
        }
    }
    assert_eq!(
        planned,
        BTreeSet::from([
            "mprime-avx2-36k-248k",
            "mprime-avx512-36k-248k",
            "mprime-sse-4k-21k"
        ])
    );
}

fn classify(line: &str) -> Line {
    Mprime::new("").unwrap().classify(line)
}

#[test]
fn every_computation_alternative_is_recognised_in_any_case() {
    for line in [
        "FATAL ERROR: Rounding was 0.5, expected less than 0.4",
        "ERROR: ILLEGAL SUMOUT",
        "Possible hardware failure",
        "Hardware failure detected running 288K FFT size, consult stress.txt file.",
        "Maximum number of warnings exceeded",
        "TORTURE TEST FAILED",
        "Torture Test completed 5000 iterations - 2 errors",
        "Torture Test completed 4 tests in 1 minutes - 12 errors, 0 warnings.",
        "ERROR: SUM(INPUTS) != SUM(OUTPUTS)",
        "ERROR: Shift counter corrupt",
        "ERROR: Illegal double encountered",
        "ERROR: FFT data has been zeroed",
        "ERROR: Jacobi error check failed",
        "Warning: ILLEGAL SUMOUT",
        "Warning: SUMOUT MISMATCH",
    ] {
        for variant in [line.to_owned(), line.to_lowercase(), line.to_uppercase()] {
            assert_eq!(classify(&variant), Line::ComputationError, "{variant}");
        }
    }
}

#[test]
fn every_setup_alternative_is_recognised() {
    for line in [
        "Error allocating memory for FFT data.",
        "Out of memory!",
        "Unable to allocate memory.  One possible cause is the operating system's swap area is too small.",
        "Cannot initialize FFT code, errcode=1002",
    ] {
        for variant in [line.to_owned(), line.to_lowercase(), line.to_uppercase()] {
            assert_eq!(classify(&variant), Line::SetupError, "{variant}");
        }
    }
}

#[test]
fn progress_keeps_the_reported_spelling() {
    for (line, want) in [
        ("Self-test 4K passed!", "self-test 4K passed"),
        (
            "[Worker #1 Sep 24 21:43] Self-test 21K (thread 1 of 2) passed!",
            "self-test 21K passed",
        ),
        ("Self-test 288 passed", "self-test 288 passed"),
        ("SELF-TEST 8k PASSED", "self-test 8k passed"),
        (
            "Self-test 6K (thread 12 of 16) passed",
            "self-test 6K passed",
        ),
    ] {
        assert_eq!(classify(line), Line::Progress(want.to_owned()), "{line}");
    }
}

#[test]
fn ordinary_lines_are_other() {
    for line in [
        "",
        "Torture Test completed 100 - 0 errors",
        "Torture Test completed 4 tests in 1 minutes - 0 errors, 0 warnings.",
        "Self-test 4K failed",
        "Self-test passed",
        "[Worker #1] Test 1, 1600000 Lucas-Lehmer in-place iterations of M83839 using FFT length 4K.",
    ] {
        assert_eq!(classify(line), Line::Other, "{line:?}");
    }
}

#[test]
fn failure_outranks_setup_outranks_progress() {
    assert_eq!(
        classify("Self-test 4K passed! FATAL ERROR Out of memory"),
        Line::ComputationError
    );
    assert_eq!(
        classify("Self-test 4K passed! Out of memory"),
        Line::SetupError
    );
    assert_eq!(
        classify("Self-test 4K passed! Warning: SUMOUT MISMATCH"),
        Line::ComputationError
    );
}

fn kinds(file: &str) -> Vec<Line> {
    let data = fs::read_to_string(format!("{TESTDATA}/{file}")).unwrap();
    data.split(['\n', '\r'])
        .filter(|line| !line.is_empty())
        .map(classify)
        .collect()
}

#[test]
fn pass_stdout_fixture_progresses_without_errors() {
    let lines = kinds("pass-stdout.txt");
    for size in ["4K", "5K", "6K", "8K"] {
        assert!(lines.contains(&Line::Progress(format!("self-test {size} passed"))));
    }
    assert!(!lines.contains(&Line::ComputationError));
    assert!(!lines.contains(&Line::SetupError));
}

#[test]
fn pass_results_fixture_has_no_errors() {
    let lines = kinds("pass-results.txt");
    assert_eq!(
        lines
            .iter()
            .filter(|l| matches!(l, Line::Progress(_)))
            .count(),
        4
    );
    assert!(!lines.contains(&Line::ComputationError));
}

#[test]
fn spliced_errors_are_not_erased_by_later_success() {
    let lines = kinds("error-spliced.txt");
    assert_eq!(
        lines
            .iter()
            .filter(|l| **l == Line::ComputationError)
            .count(),
        2
    );
    assert!(lines.iter().any(|l| matches!(l, Line::Progress(_))));
}
