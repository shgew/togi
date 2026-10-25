use super::*;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use tempfile::TempDir;
use test_case::test_case;
use togi_machine::{REGIMES, workload_by_id, workloads};

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
    ids.iter().map(|&id| CpuId::from(id)).collect()
}

fn workload(base: &str) -> &'static Workload {
    workload_by_id(&format!("{base}-smt")).unwrap()
}

fn detail(pkg: &Path) -> String {
    format!("{} ({LOWEST}, {ZEN5})", pkg.display())
}

fn binary_name() -> BytesRegex {
    Patterns::new().unwrap().binary_name
}

fn select_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> (Option<&'a str>, Option<&'a str>) {
    let names: Vec<&OsStr> = names.into_iter().map(OsStr::new).collect();
    let (lowest, zen5) = select(&binary_name(), names);
    (
        lowest.map(|name| name.to_str().unwrap()),
        zen5.map(|name| name.to_str().unwrap()),
    )
}

fn discover_names(ycruncher: &Ycruncher, names: &[&str]) -> Result<Binaries, Error> {
    ycruncher.discover_with(
        |root| {
            assert_eq!(root, ycruncher.pkg.join(BINARIES));
            Ok(names.iter().map(|name| OsString::from(*name)).collect())
        },
        |name, path| {
            assert_eq!(name, NAME);
            assert!(path.starts_with(ycruncher.pkg.join(BINARIES)));
            Ok(())
        },
    )
}

fn memory_binaries() -> Result<Binaries, Error> {
    discover_names(
        &Ycruncher::new("/pkg").unwrap(),
        &["11-SNB ~ Hina", LOWEST, ZEN5, "Digits"],
    )
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
            select_names(rotated),
            (Some("05-A64 ~ Awa"), Some("24-ZN5 ~ Alpha")),
            "rotation {rotation}"
        );
    }
    let reversed: Vec<_> = names.into_iter().rev().collect();
    assert_eq!(
        select_names(reversed),
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
    assert_eq!(select_names(names), (None, None));
    assert_eq!(
        select_names(names.into_iter().chain([ZEN5])),
        (Some(ZEN5), Some(ZEN5))
    );
}

#[test]
fn select_can_find_zen5_without_a_lower_binary() {
    assert_eq!(select_names(["24-ZN5 ~ Komari"]), (Some(ZEN5), Some(ZEN5)));
    assert_eq!(select_names(["05-A64 ~ Kasumi"]), (Some(LOWEST), None));
    assert_eq!(select_names([]), (None, None));
}

#[test]
fn identity_and_detail() {
    let pkg = Path::new("/pkg");
    let ycruncher = Ycruncher::new(pkg).unwrap();
    assert_eq!(ycruncher.kind(), togi_machine::Backend::Ycruncher);
    assert_eq!(
        ycruncher.check_with(memory_binaries).unwrap().detail,
        detail(pkg)
    );
}

#[test_case("00-README"; "readme")]
#[test_case("24-ZN5"; "prefix without a name")]
#[test_case("Digits"; "digits directory")]
fn unrelated_entries_are_ignored(name: &str) {
    let ycruncher = Ycruncher::new("/pkg").unwrap();
    assert_eq!(
        ycruncher
            .check_with(|| discover_names(&ycruncher, &[LOWEST, ZEN5, name]))
            .unwrap()
            .detail,
        detail(Path::new("/pkg"))
    );
}

#[test]
fn discovery_order_does_not_decide_the_choice() {
    let ycruncher = Ycruncher::new("/pkg").unwrap();
    assert_eq!(
        ycruncher
            .check_with(|| {
                discover_names(
                    &ycruncher,
                    &["24-ZN5 ~ Zeta", ZEN5, "11-SNB ~ Hina", LOWEST],
                )
            })
            .unwrap()
            .detail,
        detail(Path::new("/pkg"))
    );
}

#[test_case(None; "both pass")]
#[test_case(Some(LOWEST); "lowest fails")]
#[test_case(Some(ZEN5); "zen5 fails")]
fn discovery_checks_lowest_then_zen5_and_stops_on_failure(failed: Option<&str>) {
    let ycruncher = Ycruncher::new("/pkg").unwrap();
    let root = ycruncher.pkg.join(BINARIES);
    let mut checked = Vec::new();
    let result = ycruncher.discover_with(
        |_| Ok(vec![OsString::from(ZEN5), OsString::from(LOWEST)]),
        |name, path| {
            assert_eq!(name, NAME);
            checked.push(path.to_owned());
            if failed.is_some_and(|failed| path == root.join(failed)) {
                Err(Error::NotExecutable {
                    name,
                    path: path.to_owned(),
                })
            } else {
                Ok(())
            }
        },
    );
    let expected = if failed == Some(LOWEST) {
        vec![root.join(LOWEST)]
    } else {
        vec![root.join(LOWEST), root.join(ZEN5)]
    };
    assert_eq!(checked, expected);
    match failed {
        Some(failed) => assert!(matches!(
            result,
            Err(Error::NotExecutable { path, .. }) if path == root.join(failed)
        )),
        None => assert!(result.is_ok()),
    }
}

#[test_case(io::ErrorKind::NotFound; "not found")]
#[test_case(io::ErrorKind::PermissionDenied; "permission denied")]
fn directory_read_errors_precede_executable_checks(kind: io::ErrorKind) {
    let ycruncher = Ycruncher::new("/pkg").unwrap();
    let error = ycruncher
        .discover_with(
            |root| {
                assert_eq!(root, ycruncher.pkg.join(BINARIES));
                Err(kind.into())
            },
            |_, _| panic!("failed directory reads must precede executable checks"),
        )
        .unwrap_err();
    match error {
        Error::BinariesMissing { source, .. } => {
            assert_eq!(kind, io::ErrorKind::NotFound);
            assert_eq!(source.kind(), kind);
        }
        Error::ReadBinaries { source, .. } => {
            assert_eq!(kind, io::ErrorKind::PermissionDenied);
            assert_eq!(source.kind(), kind);
        }
        other @ (Error::Missing { .. }
        | Error::Stat { .. }
        | Error::NotExecutable { .. }
        | Error::NoBinaries { .. }
        | Error::NoZen5 { .. }
        | Error::Unsupported { .. }
        | Error::NoCpus { .. }
        | Error::Pattern { .. }) => panic!("{other:?}"),
    }
}

fn os(bytes: &[u8]) -> &OsStr {
    OsStr::from_bytes(bytes)
}

#[test]
fn select_compares_non_utf8_names_bytewise() {
    let odd_zen5 = os(b"24-ZN5 ~ Komari\xff");
    let odd_lowest = os(b"00-A64 ~ \xfe");
    let ignored = os(b"00-\xff ~ odd");
    let names = [
        os(ZEN5.as_bytes()),
        odd_zen5,
        odd_lowest,
        ignored,
        os(LOWEST.as_bytes()),
    ];
    assert_eq!(
        select(&binary_name(), names),
        (Some(odd_lowest), Some(os(ZEN5.as_bytes())))
    );
    assert_eq!(
        select(&binary_name(), [odd_zen5, ignored]),
        (Some(odd_zen5), Some(odd_zen5))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn real_fs_non_utf8_names_are_selected_and_checked() {
    let pkg = TempDir::new().unwrap();
    let root = binaries_dir(&pkg);
    fs::create_dir_all(&root).unwrap();
    let odd_zen5 = os(b"24-ZN5 ~ Komari\xff");
    let odd_lowest = os(b"00-A64 ~ Kasumi\xfe");
    for name in [odd_zen5, odd_lowest] {
        let path = root.join(name);
        fs::write(&path, "binary").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    // A non-matching non-UTF-8 name is still ignored, however small it sorts.
    fs::write(root.join(os(b"00-\xff ~ odd")), "binary").unwrap();
    let ycruncher = Ycruncher::new(pkg.path()).unwrap();
    assert_eq!(
        ycruncher.check().unwrap().detail,
        format!(
            "{} ({}, {})",
            pkg.path().display(),
            Path::new(odd_lowest).display(),
            Path::new(odd_zen5).display()
        )
    );
    let plan = ycruncher
        .plan(
            workload("ycruncher-fftv4-n63-vt3"),
            Path::new("/w"),
            &cpus(&[2]),
        )
        .unwrap();
    assert_eq!(plan.program, root.join(odd_zen5));
    let plan = ycruncher
        .plan(
            workload("ycruncher-bkt-sftv4"),
            Path::new("/w"),
            &cpus(&[2]),
        )
        .unwrap();
    assert_eq!(plan.program, root.join(odd_lowest));

    fs::set_permissions(root.join(odd_zen5), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        ycruncher.check().unwrap_err(),
        Error::NotExecutable { path, .. } if path == root.join(odd_zen5)
    ));
}

#[test]
fn only_a_non_matching_non_utf8_name_is_no_binary() {
    let ycruncher = Ycruncher::new("/pkg").unwrap();
    let error = ycruncher
        .discover_with(
            |_| Ok(vec![os(b"00-\xff ~ odd").to_owned()]),
            |_, _| panic!("nonmatching names must not be checked"),
        )
        .unwrap_err();
    assert!(matches!(error, Error::NoBinaries { .. }), "{error:?}");
}

#[test_case(LOWEST, false; "lowest without execute bit")]
#[test_case(ZEN5, false; "zen5 without execute bit")]
#[test_case(LOWEST, true; "lowest is a directory")]
#[test_case(ZEN5, true; "zen5 is a directory")]
fn real_fs_both_chosen_binaries_must_be_executable_regular_files(name: &str, directory: bool) {
    let pkg = fake_package(true);
    let path = binaries_dir(&pkg).join(name);
    if directory {
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
    } else {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    let error = Ycruncher::new(pkg.path()).unwrap().check().unwrap_err();
    assert!(
        matches!(&error, Error::NotExecutable { path: named, .. } if *named == path),
        "{error:?}"
    );
    assert!(!error.is_missing());
    assert!(error.to_string().contains(&path.display().to_string()));
}

#[test]
fn real_fs_missing_binaries_directory_is_backend_missing() {
    let pkg = fake_package(true);
    fs::remove_dir_all(binaries_dir(&pkg)).unwrap();
    let error = Ycruncher::new(pkg.path()).unwrap().check().unwrap_err();
    assert!(matches!(error, Error::BinariesMissing { .. }));
    assert!(error.is_missing());
    assert!(
        error
            .to_string()
            .contains(&binaries_dir(&pkg).display().to_string())
    );
}

fn without_zen5() -> Result<Binaries, Error> {
    discover_names(&Ycruncher::new("/pkg").unwrap(), &["11-SNB ~ Hina", LOWEST])
}

#[test]
fn missing_zen5_fails_check() {
    let error = Ycruncher::new("/pkg")
        .unwrap()
        .check_with(without_zen5)
        .unwrap_err();
    assert!(matches!(error, Error::NoZen5 { .. }));
    assert!(error.is_missing());
    assert!(error.to_string().contains("24-ZN5"));
}

#[test_case("ycruncher-bkt-sftv4"; "lowest binary workload")]
#[test_case("ycruncher-fftv4-n63-vt3"; "zen5 binary workload")]
fn missing_zen5_fails_every_plan(base: &str) {
    let error = Ycruncher::new("/pkg")
        .unwrap()
        .plan_with(workload(base), Path::new("/w"), &cpus(&[2]), without_zen5)
        .unwrap_err();
    assert!(matches!(error, Error::NoZen5 { .. }));
}

#[test]
fn no_matching_names_is_backend_missing() {
    let ycruncher = Ycruncher::new("/pkg").unwrap();
    let root = ycruncher.pkg.join(BINARIES);
    let error = discover_names(&ycruncher, &["README"]).unwrap_err();
    assert!(matches!(error, Error::NoBinaries { .. }));
    assert!(error.is_missing());
    assert!(error.to_string().contains(&root.display().to_string()));
}

#[test]
fn real_fs_dangling_chosen_binary_is_backend_missing() {
    let pkg = fake_package(true);
    let path = binaries_dir(&pkg).join(LOWEST);
    fs::remove_file(&path).unwrap();
    symlink("absent", &path).unwrap();
    let error = Ycruncher::new(pkg.path()).unwrap().check().unwrap_err();
    assert!(matches!(error, Error::Missing { .. }));
    assert!(error.is_missing());
}

#[test]
fn real_fs_cyclic_chosen_binary_is_not_backend_missing() {
    let pkg = fake_package(true);
    let path = binaries_dir(&pkg).join(LOWEST);
    fs::remove_file(&path).unwrap();
    symlink(LOWEST, &path).unwrap();
    let error = Ycruncher::new(pkg.path()).unwrap().check().unwrap_err();
    assert!(matches!(error, Error::Stat { .. }));
    assert!(!error.is_missing());
    assert!(error.to_string().contains(&path.display().to_string()));
}

#[test]
fn real_fs_binaries_path_that_is_a_file_is_not_backend_missing() {
    let pkg = fake_package(true);
    let root = binaries_dir(&pkg);
    fs::remove_dir_all(&root).unwrap();
    fs::write(&root, "").unwrap();
    let error = Ycruncher::new(pkg.path()).unwrap().check().unwrap_err();
    assert!(matches!(error, Error::ReadBinaries { .. }));
    assert!(!error.is_missing());
    assert!(error.to_string().contains(&root.display().to_string()));
}

#[test]
fn real_fs_unreadable_binaries_directory_is_not_backend_missing() {
    let pkg = fake_package(true);
    let root = binaries_dir(&pkg);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
    // Root, or a process with CAP_DAC_OVERRIDE, reads the directory anyway.
    let unrestricted = fs::read_dir(&root).is_ok();
    let result = Ycruncher::new(pkg.path()).unwrap().check();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    if unrestricted {
        return;
    }
    let error = result.unwrap_err();
    let Error::ReadBinaries { source, .. } = &error else {
        panic!("{error:?}");
    };
    assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
    assert!(!error.is_missing());
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

#[test_case("ycruncher-bkt-sftv4", LOWEST, &["BKT", "SFTv4"], &[2], "2", 33_554_432; "bkt sftv4 on the lowest binary")]
#[test_case("ycruncher-snt-svt", LOWEST, &["SNT", "SVT"], &[2, 18], "2 18", 67_108_864; "snt svt on two cpus")]
#[test_case("ycruncher-fftv4-n63-vt3", ZEN5, &["FFTv4", "N63", "VT3"], &[2], "2", 33_554_432; "fftv4 n63 vt3 on the zen5 binary")]
fn plans_match_exactly(
    base: &str,
    binary: &str,
    tests: &[&str],
    ids: &[u16],
    text: &str,
    memory: u64,
) {
    let pkg = Path::new("/pkg");
    let dir = Path::new("/w");
    let got = Ycruncher::new(pkg)
        .unwrap()
        .plan_with(workload(base), dir, &cpus(ids), memory_binaries)
        .unwrap();
    let want = Plan {
        program: pkg.join(BINARIES).join(binary),
        args: ["skip-warnings", "pause:-2", "status:none", "config"]
            .into_iter()
            .map(OsString::from)
            .chain([dir.join("stress.cfg").into_os_string()])
            .collect(),
        inputs: vec![Input {
            name: "stress.cfg".parse().unwrap(),
            contents: expected_config(text, memory, tests).into_bytes(),
        }],
        watched: Vec::new(),
    };
    assert_eq!(got, want);
}

#[test]
fn config_is_literal_native_syntax() {
    let plan = Ycruncher::new("/pkg")
        .unwrap()
        .plan_with(
            workload("ycruncher-snt-svt"),
            Path::new("/w"),
            &cpus(&[18, 2]),
            memory_binaries,
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
    let ycruncher = Ycruncher::new("/pkg").unwrap();
    for count in 1..=4u16 {
        let ids: Vec<u16> = (0..count).collect();
        let plan = ycruncher
            .plan_with(
                workload("ycruncher-bkt-sftv4"),
                Path::new("/w"),
                &cpus(&ids),
                memory_binaries,
            )
            .unwrap();
        let contents = String::from_utf8(plan.inputs[0].contents.clone()).unwrap();
        assert!(
            contents.contains(&format!(
                "TotalMemory : {}\n",
                33_554_432 * u64::from(count)
            )),
            "{count} cpus"
        );
    }
}

#[test]
fn unsupported_workload_is_refused_before_discovery() {
    let error = Ycruncher::new("/pkg")
        .unwrap()
        .plan_with(
            workload("mprime-sse-4k-21k"),
            Path::new("/w"),
            &cpus(&[2]),
            || panic!("unsupported workload must precede discovery"),
        )
        .unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }));
    assert!(!error.is_missing());
    assert!(error.to_string().contains("mprime-sse-4k-21k-smt"));
}

#[test]
fn a_plan_needs_a_cpu() {
    let error = Ycruncher::new("/pkg")
        .unwrap()
        .plan_with(
            workload("ycruncher-bkt-sftv4"),
            Path::new("/w"),
            &[],
            || panic!("CPU validation must precede discovery"),
        )
        .unwrap_err();
    assert!(matches!(error, Error::NoCpus { .. }));
}

#[test]
fn planning_only_discovers_binaries() {
    let mut discoveries = 0;
    let plan = Ycruncher::new("/pkg")
        .unwrap()
        .plan_with(
            workload("ycruncher-bkt-sftv4"),
            Path::new("/w"),
            &cpus(&[2]),
            || {
                discoveries += 1;
                memory_binaries()
            },
        )
        .unwrap();
    assert_eq!(discoveries, 1);
    assert_eq!(plan.inputs.len(), 1);
    assert_eq!(plan.inputs[0].name, CONFIG);
}

#[test]
fn every_catalog_workload_plans() {
    let pkg = Path::new("/pkg");
    let ycruncher = Ycruncher::new(pkg).unwrap();
    let dir = Path::new("/w");
    let mut planned = BTreeSet::new();
    for regime in REGIMES {
        for workload in workloads(regime) {
            if workload.backend != togi_machine::Backend::Ycruncher {
                continue;
            }
            let ids = [2, 18];
            let plan = ycruncher
                .plan_with(
                    workload,
                    dir,
                    &cpus(&ids[..usize::from(workload.threads)]),
                    memory_binaries,
                )
                .unwrap_or_else(|error| panic!("{regime}/{}: {error}", workload.id));
            assert!(plan.program.is_absolute());
            assert!(plan.program.starts_with(pkg.join(BINARIES)));
            assert!(
                [LOWEST, ZEN5]
                    .iter()
                    .any(|name| plan.program.ends_with(name))
            );
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
    Ycruncher::new("").unwrap().classify(line)
}

#[test_case("\x1b[31mFailed to set core affinity to core:  3\x1b[0m", Some(3); "coloured line")]
#[test_case("Failed to set core affinity to core: 3", Some(3); "plain line")]
#[test_case("failed to set core affinity to core:3", Some(3); "lower case without space")]
#[test_case("FAILED TO SET CORE AFFINITY TO CORE: 17", Some(17); "upper case")]
#[test_case("Failed to set core affinity to core: \x1b[1;31m65535", Some(65535); "coloured largest cpu")]
fn affinity_cpu_is_parsed_after_stripping_colour(line: &str, want: Option<u16>) {
    assert_eq!(
        classify(line),
        Line::AffinityError(want.map(CpuId::from)),
        "{line:?}"
    );
}

#[test_case("65536"; "cpu past u16")]
#[test_case("99999999999999999999999"; "cpu past u64")]
fn overflowing_affinity_cpu_is_a_typed_observation_error(value: &str) {
    let line = format!("Failed to set core affinity to core: {value}");
    let Line::InvalidAffinity(error) = classify(&line) else {
        panic!("recognized overflowing CPU was not refused");
    };
    assert_eq!(error.value, value);
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<std::num::ParseIntError>()
    );
}

#[test]
fn affinity_without_a_number_is_not_an_affinity_error() {
    assert_eq!(
        classify("Failed to set core affinity to core: x"),
        Line::Other
    );
}

#[test_case("Error(s) encountered on logical core 3."; "errors on a core")]
#[test_case("Coefficient is too large"; "coefficient too large")]
#[test_case("Checksum mismatch"; "checksum mismatch")]
#[test_case("Running BKT: FAIL"; "test fail")]
#[test_case("Running  SFTv4:FAIL"; "fail without space")]
#[test_case("Running\tN63:  Fail  Test Speed"; "tab and speed")]
#[test_case("running bkt: fail"; "lower case")]
#[test_case("\x1b[01;37mRunning BKT: \x1b[01;31mFAIL\x1b[01;37m"; "coloured fail")]
fn every_computation_alternative_is_recognised(line: &str) {
    assert_eq!(classify(line), Line::ComputationError, "{line:?}");
}

#[test_case("InvalidParametersException"; "exception name")]
#[test_case("Exception Encountered: InvalidParametersException"; "exception encountered")]
#[test_case("Invalid Parameter: NOPE"; "invalid parameter named")]
#[test_case("invalid parameter"; "lower case invalid parameter")]
fn every_setup_alternative_is_recognised(line: &str) {
    assert_eq!(classify(line), Line::SetupError, "{line:?}");
}

#[test_case("\x1b[32mRunning BKT: Passed\x1b[0m", "BKT passed"; "coloured pass")]
#[test_case("Running FFTv4: Passed  Test Speed:  3.9 * 10^09  bits / sec", "FFTv4 passed"; "pass with speed")]
#[test_case("running snt:passed", "snt passed"; "lower case without space")]
#[test_case("Running   VT3:   PASSED", "VT3 passed"; "upper case with spaces")]
fn progress_names_the_test(line: &str, want: &str) {
    assert_eq!(classify(line), Line::Progress(want.to_owned()), "{line:?}");
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

#[test_case(""; "empty")]
#[test_case("Insufficient permissions to set thread priority. Please retry as root."; "priority warning")]
#[test_case("Running from console..."; "console banner")]
#[test_case("Running as unit: run-p419937-i368450.scope; invocation ID: c9c3fd24bde74cea91b442193412b008"; "systemd-run banner")]
#[test_case("Running BKT: Started"; "test started")]
#[test_case("\x1b[39;49m\x1b[01;37m"; "only colour codes")]
fn ordinary_lines_are_other(line: &str) {
    assert_eq!(classify(line), Line::Other, "{line:?}");
}

#[test]
fn precedence_is_affinity_computation_setup_progress() {
    assert_eq!(
        classify("Failed to set core affinity to core: 3 Error(s) encountered"),
        Line::AffinityError(Some(CpuId::from(3)))
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

fn fixture(data: &str) -> Vec<Line> {
    data.split(['\n', '\r'])
        .filter(|line| !line.is_empty())
        .map(classify)
        .collect()
}

fn progress(test: &str) -> Line {
    Line::Progress(format!("{test} passed"))
}

#[test_case(include_str!("../testdata/lowest-pass.txt"), "BKT"; "lowest-pass.txt")]
#[test_case(include_str!("../testdata/snt-pass.txt"), "SNT"; "snt-pass.txt")]
#[test_case(include_str!("../testdata/zen5-pass.txt"), "FFTv4"; "zen5-pass.txt")]
fn passing_fixtures_report_progress(data: &str, test: &str) {
    let lines = fixture(data);
    assert!(lines.contains(&progress(test)));
    for failure in [Line::ComputationError, Line::SetupError] {
        assert!(!lines.contains(&failure), "{failure:?}");
    }
    assert!(
        !lines
            .iter()
            .any(|line| matches!(line, Line::AffinityError(_)))
    );
}

#[test]
fn setup_fixture_reports_setup_error() {
    assert!(fixture(include_str!("../testdata/setup-nope.txt")).contains(&Line::SetupError));
}

#[test]
fn affinity_fixture_reports_the_cpu() {
    let lines = fixture(include_str!("../testdata/affinity-warning.txt"));
    assert!(lines.contains(&Line::AffinityError(Some(CpuId::from(3)))));
    assert!(lines.contains(&progress("FFTv4")));
}

#[test]
fn spliced_errors_are_not_erased_by_earlier_success() {
    let lines = fixture(include_str!("../testdata/error-spliced.txt"));
    assert_eq!(
        lines
            .iter()
            .filter(|l| **l == Line::ComputationError)
            .count(),
        2
    );
    assert!(lines.contains(&progress("BKT")));
}

fn ycruncher_workloads() -> impl Iterator<Item = &'static Workload> {
    REGIMES
        .into_iter()
        .flat_map(workloads)
        .filter(|workload| workload.backend == togi_machine::Backend::Ycruncher)
}

#[test]
fn configuration_is_the_plan_without_the_directory_for_the_workloads_threads() {
    let ycruncher = Ycruncher::new("/pkg").unwrap();
    let args = [
        "skip-warnings",
        "pause:-2",
        "status:none",
        "config",
        "stress.cfg",
    ]
    .map(OsString::from);
    for workload in ycruncher_workloads() {
        let plan = ycruncher
            .plan_with(
                workload,
                Path::new("/anywhere"),
                &togi_backend::numbered_cpus(workload.threads),
                memory_binaries,
            )
            .unwrap();
        assert_eq!(
            ycruncher.configuration(workload).unwrap(),
            configuration_bytes(&args, &plan.inputs),
            "{}",
            workload.id
        );
    }
}

#[test]
fn configuration_names_neither_a_directory_nor_a_cpu_list_of_a_plan() {
    let ycruncher = Ycruncher::new("/pkg").unwrap();
    let workload = workload("ycruncher-bkt-sftv4");
    let configuration = ycruncher.configuration(workload).unwrap();
    for named in ["/one", "/two", "/pkg", "LogicalCores : [7 23]"] {
        assert!(
            !configuration
                .windows(named.len())
                .any(|window| window == named.as_bytes()),
            "{named}"
        );
    }
    let here = ycruncher
        .plan_with(
            workload,
            Path::new("/one"),
            &cpus(&[7, 23]),
            memory_binaries,
        )
        .unwrap();
    let planned = String::from_utf8(here.inputs[0].contents.clone()).unwrap();
    assert!(planned.contains("LogicalCores : [7 23]"));
    assert_eq!(configuration, ycruncher.configuration(workload).unwrap());
}

#[test]
fn configuration_differs_exactly_between_workloads_that_generate_different_input() {
    let ycruncher = Ycruncher::new("/pkg").unwrap();
    let mut by_input = std::collections::BTreeMap::new();
    for workload in ycruncher_workloads() {
        let configuration = ycruncher.configuration(workload).unwrap();
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
    let other = workload_by_id("mprime-sse-4k-21k").unwrap();
    assert!(matches!(
        Ycruncher::new("/pkg").unwrap().configuration(other),
        Err(Error::Unsupported { .. })
    ));
}
