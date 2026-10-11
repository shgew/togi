use super::*;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use tempfile::TempDir;
use togi_machine::{REGIMES, workload_by_id, workloads};

const TESTDATA: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../internal/backend/ycruncher/testdata"
);
const LOWEST: &str = "05-A64 ~ Kasumi";
const ZEN5: &str = "24-ZN5 ~ Komari";

fn binaries_dir(pkg: &TempDir) -> PathBuf {
    pkg.path().join(BINARIES)
}

fn write_binary(pkg: &TempDir, name: &str, mode: u32) -> PathBuf {
    let path = binaries_dir(pkg).join(name);
    fs::write(&path, "binary").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

fn fake_package(zen5: bool) -> TempDir {
    let pkg = TempDir::new().unwrap();
    fs::create_dir_all(binaries_dir(&pkg)).unwrap();
    write_binary(&pkg, "11-SNB ~ Hina", 0o755);
    write_binary(&pkg, LOWEST, 0o755);
    if zen5 {
        write_binary(&pkg, ZEN5, 0o755);
    }
    fs::create_dir(binaries_dir(&pkg).join("Digits")).unwrap();
    pkg
}

fn cpus(ids: &[u16]) -> Vec<CpuId> {
    ids.iter().map(|&id| CpuId(id)).collect()
}

fn workload(base: &str) -> &'static Workload {
    workload_by_id(&format!("{base}-smt")).unwrap()
}

fn detail(pkg: &TempDir) -> String {
    format!("{} ({LOWEST}, {ZEN5})", pkg.path().display())
}

#[test]
fn select_picks_the_lexical_minimum_whatever_the_order() {
    let names = [
        "24-ZN5 ~ Zeta",
        "11-SNB ~ Hina",
        "24-ZN5 ~ Alpha",
        "05-A64 ~ Kasumi",
        "05-A64 ~ Awa",
    ];
    for rotation in 0..names.len() {
        let mut rotated = names;
        rotated.rotate_left(rotation);
        assert_eq!(
            select(rotated),
            (Some("05-A64 ~ Awa"), Some("24-ZN5 ~ Alpha"))
        );
    }
    let reversed: Vec<_> = names.into_iter().rev().collect();
    assert_eq!(
        select(reversed),
        (Some("05-A64 ~ Awa"), Some("24-ZN5 ~ Alpha"))
    );
}

#[test]
fn select_ignores_names_that_are_not_binaries() {
    let names = [
        "00-README",
        "Digits",
        "24-ZN5",
        "24-ZN5 ~ ",
        "05-A64~Kasumi",
        "5-A64 ~ Kasumi",
        "005-A64 ~ Kasumi",
        "05-A_64 ~ Kasumi",
        "05-A64 ~ ",
        "05--A64 ~ Kasumi",
        "x05-A64 ~ Kasumi",
        "05-A64 ~ Kasumi\nmore",
        "",
    ];
    assert_eq!(select(names), (None, None));
    assert_eq!(
        select(names.into_iter().chain([ZEN5])),
        (Some(ZEN5), Some(ZEN5))
    );
}

#[test]
fn select_can_find_zen5_without_a_lower_binary() {
    assert_eq!(select(["24-ZN5 ~ Komari"]), (Some(ZEN5), Some(ZEN5)));
    assert_eq!(select(["05-A64 ~ Kasumi"]), (Some(LOWEST), None));
    assert_eq!(select([]), (None, None));
}

#[test]
fn identity_and_detail() {
    let pkg = fake_package(true);
    let ycruncher = Ycruncher::new(pkg.path());
    assert_eq!(ycruncher.kind(), togi_machine::Backend::Ycruncher);
    assert_eq!(ycruncher.check().unwrap().detail, detail(&pkg));
}

#[test]
fn unrelated_entries_are_ignored() {
    for (name, mode) in [
        ("00-README", 0o644),
        ("00-README", 0o755),
        ("24-ZN5", 0o755),
    ] {
        let pkg = fake_package(true);
        write_binary(&pkg, name, mode);
        assert_eq!(
            Ycruncher::new(pkg.path()).check().unwrap().detail,
            detail(&pkg),
            "{name} {mode:o}"
        );
    }
}

#[test]
fn creation_order_does_not_decide_the_choice() {
    let pkg = TempDir::new().unwrap();
    fs::create_dir_all(binaries_dir(&pkg)).unwrap();
    write_binary(&pkg, "24-ZN5 ~ Zeta", 0o755);
    write_binary(&pkg, "24-ZN5 ~ Komari", 0o755);
    write_binary(&pkg, "11-SNB ~ Hina", 0o755);
    write_binary(&pkg, "05-A64 ~ Kasumi", 0o755);
    assert_eq!(
        Ycruncher::new(pkg.path()).check().unwrap().detail,
        detail(&pkg)
    );
}

#[test]
fn non_utf8_names_are_ignored() {
    let pkg = fake_package(true);
    let odd = binaries_dir(&pkg).join(OsStr::from_bytes(b"00-\xff ~ odd"));
    fs::write(&odd, "binary").unwrap();
    assert_eq!(
        Ycruncher::new(pkg.path()).check().unwrap().detail,
        detail(&pkg)
    );
}

#[test]
fn both_chosen_binaries_must_be_executable_regular_files() {
    for (name, directory) in [(LOWEST, false), (ZEN5, false), (LOWEST, true), (ZEN5, true)] {
        let pkg = fake_package(true);
        let path = binaries_dir(&pkg).join(name);
        if directory {
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
        } else {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        let error = Ycruncher::new(pkg.path()).check().unwrap_err();
        assert!(
            matches!(&error, Error::NotExecutable { path: named, .. } if *named == path),
            "{name} directory={directory}: {error:?}"
        );
        assert!(!error.is_missing());
        assert!(error.to_string().contains(&path.display().to_string()));
    }
}

#[test]
fn missing_binaries_directory_is_backend_missing() {
    let pkg = fake_package(true);
    fs::remove_dir_all(binaries_dir(&pkg)).unwrap();
    let error = Ycruncher::new(pkg.path()).check().unwrap_err();
    assert!(matches!(error, Error::BinariesMissing { .. }));
    assert!(error.is_missing());
    assert!(
        error
            .to_string()
            .contains(&binaries_dir(&pkg).display().to_string())
    );
}

#[test]
fn missing_zen5_fails_check_and_every_plan() {
    let pkg = fake_package(false);
    let ycruncher = Ycruncher::new(pkg.path());
    let error = ycruncher.check().unwrap_err();
    assert!(matches!(error, Error::NoZen5 { .. }));
    assert!(error.is_missing());
    assert!(error.to_string().contains("24-ZN5"));
    for base in ["ycruncher-bkt-sftv4", "ycruncher-fftv4-n63-vt3"] {
        let error = ycruncher
            .plan(workload(base), Path::new("/w"), &cpus(&[2]))
            .unwrap_err();
        assert!(matches!(error, Error::NoZen5 { .. }), "{base}");
    }
}

#[test]
fn no_matching_names_is_backend_missing() {
    let pkg = fake_package(true);
    let root = binaries_dir(&pkg);
    fs::remove_dir_all(&root).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("README"), "").unwrap();
    let error = Ycruncher::new(pkg.path()).check().unwrap_err();
    assert!(matches!(error, Error::NoBinaries { .. }));
    assert!(error.is_missing());
    assert!(error.to_string().contains(&root.display().to_string()));
}

#[test]
fn dangling_chosen_binary_is_backend_missing() {
    let pkg = fake_package(true);
    let path = binaries_dir(&pkg).join(LOWEST);
    fs::remove_file(&path).unwrap();
    symlink("absent", &path).unwrap();
    let error = Ycruncher::new(pkg.path()).check().unwrap_err();
    assert!(matches!(error, Error::Missing { .. }));
    assert!(error.is_missing());
}

#[test]
fn cyclic_chosen_binary_is_not_backend_missing() {
    let pkg = fake_package(true);
    let path = binaries_dir(&pkg).join(LOWEST);
    fs::remove_file(&path).unwrap();
    symlink(LOWEST, &path).unwrap();
    let error = Ycruncher::new(pkg.path()).check().unwrap_err();
    assert!(matches!(error, Error::Stat { .. }));
    assert!(!error.is_missing());
    assert!(error.to_string().contains(&path.display().to_string()));
}

#[test]
fn binaries_path_that_is_a_file_is_not_backend_missing() {
    let pkg = fake_package(true);
    let root = binaries_dir(&pkg);
    fs::remove_dir_all(&root).unwrap();
    fs::write(&root, "").unwrap();
    let error = Ycruncher::new(pkg.path()).check().unwrap_err();
    assert!(matches!(error, Error::ReadBinaries { .. }));
    assert!(!error.is_missing());
    assert!(error.to_string().contains(&root.display().to_string()));
}

#[test]
fn unreadable_binaries_directory_is_not_backend_missing() {
    let pkg = fake_package(true);
    let root = binaries_dir(&pkg);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
    let result = Ycruncher::new(pkg.path()).check();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    if let Err(error) = result {
        assert!(matches!(error, Error::ReadBinaries { .. }));
        assert!(!error.is_missing());
    }
}

fn expected_config(cpus: &str, memory: u64, tests: &[&str]) -> String {
    let tests: String = tests
        .iter()
        .map(|test| ["            \"", test, "\"\n"].concat())
        .collect();
    format!(
        "{{\n    Action : \"StressTest\"\n    StressTest : {{\n        AllocateLocally : \"true\"\n        LogicalCores : [{cpus}]\n        TotalMemory : {memory}\n        SecondsPerTest : 30\n        SecondsTotal : 0\n        StopOnError : \"true\"\n        Tests : [\n{tests}        ]\n    }}\n}}\n"
    )
}

#[test]
fn plans_match_exactly() {
    let pkg = fake_package(true);
    let ycruncher = Ycruncher::new(pkg.path());
    let dir = TempDir::new().unwrap();
    for (base, binary, tests, ids, text, memory) in [
        (
            "ycruncher-bkt-sftv4",
            LOWEST,
            &["BKT", "SFTv4"][..],
            vec![2],
            "2",
            33_554_432,
        ),
        (
            "ycruncher-snt-svt",
            LOWEST,
            &["SNT", "SVT"],
            vec![2, 18],
            "2 18",
            67_108_864,
        ),
        (
            "ycruncher-fftv4-n63-vt3",
            ZEN5,
            &["FFTv4", "N63", "VT3"],
            vec![2],
            "2",
            33_554_432,
        ),
    ] {
        let got = ycruncher
            .plan(workload(base), dir.path(), &cpus(&ids))
            .unwrap();
        let want = Plan {
            program: binaries_dir(&pkg).join(binary),
            args: ["skip-warnings", "pause:-2", "status:none", "config"]
                .into_iter()
                .map(OsString::from)
                .chain([dir.path().join("stress.cfg").into_os_string()])
                .collect(),
            inputs: vec![Input {
                name: "stress.cfg".parse().unwrap(),
                contents: expected_config(text, memory, tests).into_bytes(),
            }],
            watched: Vec::new(),
        };
        assert_eq!(got, want, "{base}");
    }
}

#[test]
fn config_is_literal_native_syntax() {
    let pkg = fake_package(true);
    let plan = Ycruncher::new(pkg.path())
        .plan(
            workload("ycruncher-snt-svt"),
            Path::new("/w"),
            &cpus(&[18, 2]),
        )
        .unwrap();
    assert_eq!(
        String::from_utf8(plan.inputs[0].contents.clone()).unwrap(),
        "{\n    Action : \"StressTest\"\n    StressTest : {\n        AllocateLocally : \"true\"\n        LogicalCores : [18 2]\n        TotalMemory : 67108864\n        SecondsPerTest : 30\n        SecondsTotal : 0\n        StopOnError : \"true\"\n        Tests : [\n            \"SNT\"\n            \"SVT\"\n        ]\n    }\n}\n"
    );
    assert_eq!(plan.args.last().unwrap(), OsStr::new("/w/stress.cfg"));
}

#[test]
fn memory_is_32_mib_per_cpu() {
    let pkg = fake_package(true);
    let ycruncher = Ycruncher::new(pkg.path());
    for count in 1..=4u16 {
        let ids: Vec<u16> = (0..count).collect();
        let plan = ycruncher
            .plan(
                workload("ycruncher-bkt-sftv4"),
                Path::new("/w"),
                &cpus(&ids),
            )
            .unwrap();
        let contents = String::from_utf8(plan.inputs[0].contents.clone()).unwrap();
        assert!(contents.contains(&format!(
            "TotalMemory : {}\n",
            33_554_432 * u64::from(count)
        )));
    }
}

#[test]
fn unsupported_workload_is_refused_before_discovery() {
    let pkg = TempDir::new().unwrap();
    let error = Ycruncher::new(pkg.path())
        .plan(workload("mprime-sse-4k-21k"), Path::new("/w"), &cpus(&[2]))
        .unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }));
    assert!(!error.is_missing());
    assert!(error.to_string().contains("mprime-sse-4k-21k-smt"));
}

#[test]
fn a_plan_needs_a_cpu() {
    let pkg = fake_package(true);
    let error = Ycruncher::new(pkg.path())
        .plan(workload("ycruncher-bkt-sftv4"), Path::new("/w"), &[])
        .unwrap_err();
    assert!(matches!(error, Error::NoCpus { .. }));
}

#[test]
fn planning_writes_nothing() {
    let pkg = fake_package(true);
    let dir = TempDir::new().unwrap();
    Ycruncher::new(pkg.path())
        .plan(workload("ycruncher-bkt-sftv4"), dir.path(), &cpus(&[2]))
        .unwrap();
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn every_catalog_workload_plans() {
    let pkg = fake_package(true);
    let ycruncher = Ycruncher::new(pkg.path());
    let dir = TempDir::new().unwrap();
    let mut planned = BTreeSet::new();
    for regime in REGIMES {
        for workload in workloads(regime) {
            if workload.backend != togi_machine::Backend::Ycruncher {
                continue;
            }
            let ids = [2, 18];
            let plan = ycruncher
                .plan(
                    workload,
                    dir.path(),
                    &cpus(&ids[..usize::from(workload.threads)]),
                )
                .unwrap_or_else(|error| panic!("{regime}/{}: {error}", workload.id));
            assert!(plan.program.is_absolute());
            assert!(plan.program.starts_with(pkg.path()));
            assert!(fs::metadata(&plan.program).unwrap().permissions().mode() & 0o111 != 0);
            assert_eq!(plan.inputs.len(), 1);
            assert_eq!(plan.inputs[0].name.as_str(), "stress.cfg");
            planned.insert(workload.base);
        }
    }
    assert_eq!(
        planned,
        BTreeSet::from([
            "ycruncher-bkt-sftv4",
            "ycruncher-fftv4-n63-vt3",
            "ycruncher-snt-svt"
        ])
    );
}

fn classify(line: &str) -> Line {
    Ycruncher::new("").classify(line)
}

#[test]
fn affinity_cpu_is_parsed_after_stripping_colour() {
    for (line, want) in [
        (
            "\x1b[31mFailed to set core affinity to core:  3\x1b[0m",
            Some(3),
        ),
        ("Failed to set core affinity to core: 3", Some(3)),
        ("failed to set core affinity to core:3", Some(3)),
        ("FAILED TO SET CORE AFFINITY TO CORE: 17", Some(17)),
        (
            "Failed to set core affinity to core: \x1b[1;31m65535",
            Some(65535),
        ),
        ("Failed to set core affinity to core: 65536", None),
        (
            "Failed to set core affinity to core: 99999999999999999999999",
            None,
        ),
    ] {
        assert_eq!(
            classify(line),
            Line::AffinityError(want.map(CpuId)),
            "{line:?}"
        );
    }
}

#[test]
fn affinity_without_a_number_is_not_an_affinity_error() {
    assert_eq!(
        classify("Failed to set core affinity to core: x"),
        Line::Other
    );
}

#[test]
fn every_computation_alternative_is_recognised() {
    for line in [
        "Error(s) encountered on logical core 3.",
        "Coefficient is too large",
        "Checksum mismatch",
        "Running BKT: FAIL",
        "Running  SFTv4:FAIL",
        "Running\tN63:  Fail  Test Speed",
        "running bkt: fail",
        "\x1b[01;37mRunning BKT: \x1b[01;31mFAIL\x1b[01;37m",
    ] {
        assert_eq!(classify(line), Line::ComputationError, "{line:?}");
    }
}

#[test]
fn every_setup_alternative_is_recognised() {
    for line in [
        "InvalidParametersException",
        "Exception Encountered: InvalidParametersException",
        "Invalid Parameter: NOPE",
        "invalid parameter",
    ] {
        assert_eq!(classify(line), Line::SetupError, "{line:?}");
    }
}

#[test]
fn progress_names_the_test() {
    for (line, want) in [
        ("\x1b[32mRunning BKT: Passed\x1b[0m", "BKT passed"),
        (
            "Running FFTv4: Passed  Test Speed:  3.9 * 10^09  bits / sec",
            "FFTv4 passed",
        ),
        ("running snt:passed", "snt passed"),
        ("Running   VT3:   PASSED", "VT3 passed"),
    ] {
        assert_eq!(classify(line), Line::Progress(want.to_owned()), "{line:?}");
    }
}

#[test]
fn carriage_return_separated_progress() {
    let passed: Vec<_> = "Running BKT: Passed\rRunning SFTv4: Passed"
        .split('\r')
        .map(classify)
        .collect();
    assert_eq!(
        passed,
        [
            Line::Progress("BKT passed".to_owned()),
            Line::Progress("SFTv4 passed".to_owned())
        ]
    );
}

#[test]
fn ordinary_lines_are_other() {
    for line in [
        "",
        "Insufficient permissions to set thread priority. Please retry as root.",
        "Running from console...",
        "Running as unit: run-p419937-i368450.scope; invocation ID: c9c3fd24bde74cea91b442193412b008",
        "Running BKT: Started",
        "\x1b[39;49m\x1b[01;37m",
    ] {
        assert_eq!(classify(line), Line::Other, "{line:?}");
    }
}

#[test]
fn precedence_is_affinity_computation_setup_progress() {
    assert_eq!(
        classify("Failed to set core affinity to core: 3 Error(s) encountered"),
        Line::AffinityError(Some(CpuId(3)))
    );
    assert_eq!(
        classify("Running BKT: Passed Checksum mismatch InvalidParametersException"),
        Line::ComputationError
    );
    assert_eq!(
        classify("Running BKT: Passed InvalidParametersException"),
        Line::SetupError
    );
}

fn fixture(name: &str) -> Vec<Line> {
    fs::read_to_string(format!("{TESTDATA}/{name}"))
        .unwrap()
        .split(['\n', '\r'])
        .filter(|line| !line.is_empty())
        .map(classify)
        .collect()
}

fn progress(test: &str) -> Line {
    Line::Progress(format!("{test} passed"))
}

#[test]
fn passing_fixtures_report_progress() {
    for (name, test) in [
        ("lowest-pass.txt", "BKT"),
        ("snt-pass.txt", "SNT"),
        ("zen5-pass.txt", "FFTv4"),
    ] {
        let lines = fixture(name);
        assert!(lines.contains(&progress(test)), "{name}");
        for failure in [Line::ComputationError, Line::SetupError] {
            assert!(!lines.contains(&failure), "{name}");
        }
        assert!(
            !lines
                .iter()
                .any(|line| matches!(line, Line::AffinityError(_))),
            "{name}"
        );
    }
}

#[test]
fn setup_fixture_reports_setup_error() {
    assert!(fixture("setup-nope.txt").contains(&Line::SetupError));
}

#[test]
fn affinity_fixture_reports_the_cpu() {
    let lines = fixture("affinity-warning.txt");
    assert!(lines.contains(&Line::AffinityError(Some(CpuId(3)))));
    assert!(lines.contains(&progress("FFTv4")));
}

#[test]
fn spliced_errors_are_not_erased_by_earlier_success() {
    let lines = fixture("error-spliced.txt");
    assert_eq!(
        lines
            .iter()
            .filter(|l| **l == Line::ComputationError)
            .count(),
        2
    );
    assert!(lines.contains(&progress("BKT")));
}
