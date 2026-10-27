use super::*;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use tempfile::TempDir;
use test_case::test_case;
use togi_machine::{ALL_REGIMES, workload_by_id, workloads};

fn fake_package() -> TempDir {
    let pkg = TempDir::new().unwrap();
    fs::create_dir(pkg.path().join("bin")).unwrap();
    let binary = pkg.path().join("bin/mprime");
    fs::write(&binary, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    pkg
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the executable-check seam is fallible"
)]
fn executable(name: &'static str, path: &Path) -> Result<(), Error> {
    assert_eq!(name, NAME);
    assert_eq!(path, Path::new("/pkg/bin/mprime"));
    Ok(())
}

fn cpus(ids: &[u16]) -> Vec<CpuId> {
    ids.iter().map(|&id| CpuId::from(id)).collect()
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
    let mprime = Mprime::new("/pkg").unwrap();
    assert_eq!(mprime.kind(), togi_machine::Backend::Mprime);
    let ready = mprime
        .check_with(|name, path| {
            assert_eq!(name, NAME);
            assert_eq!(path, Path::new("/pkg/bin/mprime"));
            Ok(())
        })
        .unwrap();
    assert_eq!(ready.detail, "/pkg/bin/mprime");
}

#[test]
fn real_fs_missing_binary_is_backend_missing() {
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
fn real_fs_execute_mode_is_checked_by_check_and_plan() {
    let pkg = fake_package();
    let mprime = Mprime::new(pkg.path()).unwrap();
    assert_eq!(
        mprime.check().unwrap().detail,
        pkg.path().join("bin/mprime").display().to_string()
    );
    let dir = pkg.path().join("work");
    let workload = workload("mprime-sse-4k-21k");
    let cpus = cpus(&[2]);
    assert_eq!(
        mprime.plan(workload, &dir, &cpus).unwrap().program,
        pkg.path().join("bin/mprime")
    );
    fs::set_permissions(
        pkg.path().join("bin/mprime"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let error = mprime.check().unwrap_err();
    assert!(matches!(error, Error::NotExecutable { .. }));
    assert!(!error.is_missing());
    assert!(matches!(
        mprime.plan(workload, &dir, &cpus),
        Err(Error::NotExecutable { .. })
    ));
}

#[test]
fn real_fs_stat_failure_is_not_backend_missing() {
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

#[test_case("mprime-sse-4k-21k", (4, 21), [0, 0, 0, 0], &[2], 0; "sse one cpu")]
#[test_case("mprime-avx512-36k-248k", (36, 248), [1, 1, 1, 1], &[2, 18], 1; "avx512 two threads")]
#[test_case("mprime-avx2-36k-248k", (36, 248), [1, 1, 1, 0], &[2], 0; "avx2 one cpu")]
fn plans_match_exactly(base: &str, fft: (u16, u16), flags: [u8; 4], ids: &[u16], hyper: u8) {
    let pkg = Path::new("/pkg");
    let dir = Path::new("/w");
    let got = Mprime::new(pkg)
        .unwrap()
        .plan_with(workload(base), dir, &cpus(ids), executable)
        .unwrap();
    let settings = expected_settings(fft, flags, hyper, ids.len());
    let want = Plan {
        program: pkg.join("bin/mprime"),
        args: vec![
            OsString::from("-t"),
            OsString::from(format!("-W{}", dir.display())),
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
                contents: format!("ErrorCheck=1\nSumInputsErrorCheck=1\n{settings}").into_bytes(),
            },
        ],
        watched: vec!["results.txt".parse().unwrap()],
    };
    assert_eq!(got, want);
}

#[test]
fn sse_single_cpu_local_txt_is_literal() {
    let plan = Mprime::new("/pkg")
        .unwrap()
        .plan_with(
            workload("mprime-sse-4k-21k"),
            Path::new("/w"),
            &cpus(&[2]),
            executable,
        )
        .unwrap();
    assert_eq!(
        text(&plan, "local.txt"),
        "ErrorCheck=1\nSumInputsErrorCheck=1\nNumCPUs=1\nCoresPerTest=1\nMinTortureFFT=4\nMaxTortureFFT=21\nTortureTime=1\nTortureHyperthreading=0\nTortureThreads=1\nCpuSupportsAVX=0\nCpuSupportsFMA3=0\nCpuSupportsFMA4=0\nCpuSupportsAVX2=0\nCpuSupportsAVX512F=0\n"
    );
    assert_eq!(plan.args[1], OsString::from("-W/w"));
}

#[test_case(&[2], 0; "one cpu")]
#[test_case(&[2, 18], 1; "one core two threads")]
#[test_case(&[2, 18, 3, 19], 1; "two cores four threads")]
fn thread_count_and_hyperthreading_follow_the_cpu_list(ids: &[u16], hyper: u8) {
    let plan = Mprime::new("/pkg")
        .unwrap()
        .plan_with(
            workload("mprime-sse-4k-21k"),
            Path::new("/w"),
            &cpus(ids),
            executable,
        )
        .unwrap();
    for name in ["prime.txt", "local.txt"] {
        let contents = text(&plan, name);
        assert!(contents.contains(&format!("TortureThreads={}\n", ids.len())));
        assert!(contents.contains(&format!("TortureHyperthreading={hyper}\n")));
    }
}

#[test]
fn unsupported_workload_is_refused_before_the_binary_is_checked() {
    let error = Mprime::new("/pkg")
        .unwrap()
        .plan_with(
            workload("ycruncher-bkt-sftv4"),
            Path::new("/w"),
            &cpus(&[2]),
            |_, _| panic!("unsupported workload must precede executable validation"),
        )
        .unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }));
    assert!(!error.is_missing());
    assert!(error.to_string().contains("ycruncher-bkt-sftv4-smt"));
}

#[test]
fn a_plan_needs_a_cpu() {
    let error = Mprime::new("/pkg")
        .unwrap()
        .plan_with(
            workload("mprime-sse-4k-21k"),
            Path::new("/w"),
            &[],
            |_, _| panic!("CPU validation must precede executable validation"),
        )
        .unwrap_err();
    assert!(matches!(error, Error::NoCpus { .. }));
}

#[test]
fn missing_binary_yields_no_plan() {
    let error = Mprime::new("/pkg")
        .unwrap()
        .plan_with(
            workload("mprime-sse-4k-21k"),
            Path::new("/w"),
            &cpus(&[2]),
            |name, path| {
                Err(Error::Missing {
                    name,
                    path: path.to_owned(),
                    source: std::io::ErrorKind::NotFound.into(),
                })
            },
        )
        .unwrap_err();
    assert!(error.is_missing());
}

#[test]
fn planning_only_checks_the_executable() {
    let mut checked = Vec::new();
    let plan = Mprime::new("/pkg")
        .unwrap()
        .plan_with(
            workload("mprime-sse-4k-21k"),
            Path::new("/w"),
            &cpus(&[2]),
            |name, path| {
                checked.push((name, path.to_owned()));
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(checked, [(NAME, plan.program.clone())]);
    assert_eq!(names(&plan), ["prime.txt", "local.txt"]);
}

#[test]
fn every_catalog_workload_plans() {
    let pkg = Path::new("/pkg");
    let mprime = Mprime::new(pkg).unwrap();
    let dir = Path::new("/w");
    let mut planned = BTreeSet::new();
    for regime in ALL_REGIMES {
        for workload in workloads(regime) {
            if workload.backend != togi_machine::Backend::Mprime {
                continue;
            }
            let ids = [2, 18];
            let plan = mprime
                .plan_with(
                    workload,
                    dir,
                    &cpus(&ids[..usize::from(workload.threads)]),
                    executable,
                )
                .unwrap_or_else(|error| panic!("{regime}/{}: {error}", workload.id));
            assert!(plan.program.is_absolute(), "{regime}/{}", workload.id);
            assert_eq!(
                plan.program,
                pkg.join("bin/mprime"),
                "{regime}/{}",
                workload.id
            );
            assert_eq!(
                names(&plan),
                ["prime.txt", "local.txt"],
                "{regime}/{}",
                workload.id
            );
            assert_eq!(plan.watched.len(), 1, "{regime}/{}", workload.id);
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

fn mprime_workloads() -> impl Iterator<Item = &'static Workload> {
    ALL_REGIMES
        .into_iter()
        .flat_map(workloads)
        .filter(|workload| workload.backend == togi_machine::Backend::Mprime)
}

#[test]
fn configuration_is_the_plan_without_the_directory_for_the_workloads_threads() {
    let mprime = Mprime::new("/pkg").unwrap();
    let args = [OsString::from("-t"), OsString::from("-W")];
    for workload in mprime_workloads() {
        let plan = mprime
            .plan_with(
                workload,
                Path::new("/anywhere"),
                &togi_backend::numbered_cpus(workload.threads),
                executable,
            )
            .unwrap();
        assert_eq!(
            mprime.configuration(workload).unwrap(),
            configuration_bytes(&args, &plan.inputs),
            "{}",
            workload.id
        );
    }
}

#[test]
fn configuration_names_neither_a_directory_nor_a_cpu() {
    let mprime = Mprime::new("/pkg").unwrap();
    let workload = workload("mprime-sse-4k-21k");
    let here = mprime
        .plan_with(workload, Path::new("/one"), &cpus(&[2, 18]), executable)
        .unwrap();
    let there = mprime
        .plan_with(
            workload,
            Path::new("/two/deeper"),
            &cpus(&[7, 23]),
            executable,
        )
        .unwrap();
    assert_eq!(here.inputs, there.inputs);
    let configuration = mprime.configuration(workload).unwrap();
    for directory in ["/one", "/two", "/pkg"] {
        assert!(
            !configuration
                .windows(directory.len())
                .any(|window| window == directory.as_bytes()),
            "{directory}"
        );
    }
    assert_eq!(configuration, mprime.configuration(workload).unwrap());
}

#[test]
fn configuration_differs_exactly_between_workloads_that_generate_different_input() {
    let mprime = Mprime::new("/pkg").unwrap();
    let mut by_input = std::collections::BTreeMap::new();
    for workload in mprime_workloads() {
        let configuration = mprime.configuration(workload).unwrap();
        let known = by_input
            .entry((workload.base, workload.threads))
            .or_insert_with(|| configuration.clone());
        assert_eq!(*known, configuration, "{}", workload.id);
    }
    let distinct: BTreeSet<_> = by_input.values().collect();
    assert_eq!(distinct.len(), by_input.len());
    assert!(by_input.len() >= 4, "{by_input:?}");
}

#[test]
fn a_workload_of_another_backend_has_no_configuration() {
    let other = workload_by_id("ycruncher-bkt-sftv4").unwrap();
    assert!(matches!(
        Mprime::new("/pkg").unwrap().configuration(other),
        Err(Error::Unsupported { .. })
    ));
}

fn classify(line: &str) -> Line {
    Mprime::new("").unwrap().classify(line)
}

#[test_case("FATAL ERROR: Rounding was 0.5, expected less than 0.4"; "rounding")]
#[test_case("ERROR: ILLEGAL SUMOUT"; "illegal sumout error")]
#[test_case("Possible hardware failure"; "possible hardware failure")]
#[test_case("Hardware failure detected running 288K FFT size, consult stress.txt file."; "hardware failure detected")]
#[test_case("Maximum number of warnings exceeded"; "too many warnings")]
#[test_case("TORTURE TEST FAILED"; "torture test failed")]
#[test_case("Torture Test completed 5000 iterations - 2 errors"; "iterations with errors")]
#[test_case("Torture Test completed 4 tests in 1 minutes - 12 errors, 0 warnings."; "tests with errors")]
#[test_case("ERROR: SUM(INPUTS) != SUM(OUTPUTS)"; "sum mismatch")]
#[test_case("ERROR: Shift counter corrupt"; "shift counter")]
#[test_case("ERROR: Illegal double encountered"; "illegal double")]
#[test_case("ERROR: FFT data has been zeroed"; "zeroed data")]
#[test_case("ERROR: Jacobi error check failed"; "jacobi")]
#[test_case("Warning: ILLEGAL SUMOUT"; "illegal sumout warning")]
#[test_case("Warning: SUMOUT MISMATCH"; "sumout mismatch warning")]
fn every_computation_alternative_is_recognised_in_any_case(line: &str) {
    for variant in [line.to_owned(), line.to_lowercase(), line.to_uppercase()] {
        assert_eq!(classify(&variant), Line::ComputationError, "{variant}");
    }
}

#[test_case("Error allocating memory for FFT data."; "fft allocation")]
#[test_case("Out of memory!"; "out of memory")]
#[test_case("Unable to allocate memory.  One possible cause is the operating system's swap area is too small."; "unable to allocate")]
#[test_case("Cannot initialize FFT code, errcode=1002"; "fft initialisation")]
fn every_setup_alternative_is_recognised(line: &str) {
    for variant in [line.to_owned(), line.to_lowercase(), line.to_uppercase()] {
        assert_eq!(classify(&variant), Line::SetupError, "{variant}");
    }
}

#[test_case("Self-test 4K passed!", "self-test 4K passed"; "exclamation")]
#[test_case("[Worker #1 Sep 24 21:43] Self-test 21K (thread 1 of 2) passed!", "self-test 21K passed"; "worker prefix and thread")]
#[test_case("Self-test 288 passed", "self-test 288 passed"; "size without unit")]
#[test_case("SELF-TEST 8k PASSED", "self-test 8k passed"; "upper case")]
#[test_case("Self-test 6K (thread 12 of 16) passed", "self-test 6K passed"; "thread without exclamation")]
fn progress_keeps_the_reported_spelling(line: &str, want: &str) {
    assert_eq!(classify(line), Line::Progress(want.to_owned()), "{line}");
}

#[test_case(""; "empty")]
#[test_case("Torture Test completed 100 - 0 errors"; "zero errors")]
#[test_case("Torture Test completed 4 tests in 1 minutes - 0 errors, 0 warnings."; "tests with zero errors")]
#[test_case("Self-test 4K failed"; "self-test failed")]
#[test_case("Self-test passed"; "self-test without size")]
#[test_case("[Worker #1] Test 1, 1600000 Lucas-Lehmer in-place iterations of M83839 using FFT length 4K."; "lucas-lehmer status")]
fn ordinary_lines_are_other(line: &str) {
    assert_eq!(classify(line), Line::Other, "{line:?}");
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

fn kinds(data: &str) -> Vec<Line> {
    data.split(['\n', '\r'])
        .filter(|line| !line.is_empty())
        .map(classify)
        .collect()
}

#[test]
fn pass_stdout_fixture_progresses_without_errors() {
    let lines = kinds(include_str!("../testdata/pass-stdout.txt"));
    for size in ["4K", "5K", "6K", "8K"] {
        assert!(lines.contains(&Line::Progress(format!("self-test {size} passed"))));
    }
    assert!(!lines.contains(&Line::ComputationError));
    assert!(!lines.contains(&Line::SetupError));
}

#[test]
fn pass_results_fixture_has_no_errors() {
    let lines = kinds(include_str!("../testdata/pass-results.txt"));
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
    let lines = kinds(include_str!("../testdata/error-spliced.txt"));
    assert_eq!(
        lines
            .iter()
            .filter(|l| **l == Line::ComputationError)
            .count(),
        2
    );
    assert!(lines.iter().any(|l| matches!(l, Line::Progress(_))));
}
