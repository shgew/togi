use crate::*;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use tempfile::TempDir;

fn catalog(id: &str) -> togi_machine::WorkloadId {
    id.parse().unwrap()
}

const PLAIN: FileName = FileName::from_static("prime.txt");

fn file_with_mode(dir: &TempDir, name: &str, mode: u32) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, "binary").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

#[test]
fn file_names_are_single_components() {
    for (name, want) in [
        ("prime.txt", Ok(())),
        ("stress.cfg", Ok(())),
        (".hidden", Ok(())),
        ("...", Ok(())),
        ("with space ~ and tilde", Ok(())),
        ("", Err(InvalidFileName::Empty)),
        (".", Err(InvalidFileName::CurrentDirectory)),
        ("..", Err(InvalidFileName::ParentDirectory)),
        ("a/b", Err(InvalidFileName::Separator)),
        ("/etc", Err(InvalidFileName::Separator)),
        ("dir/", Err(InvalidFileName::Separator)),
        ("../x", Err(InvalidFileName::Separator)),
        ("a\0b", Err(InvalidFileName::Nul)),
        ("\0", Err(InvalidFileName::Nul)),
    ] {
        assert_eq!(name.parse::<FileName>().map(|_| ()), want, "{name:?}");
        assert_eq!(
            FileName::try_from(name.to_owned()).map(|_| ()),
            want,
            "{name:?}"
        );
    }
}

#[test]
fn static_names_are_validated_at_run_time_too() {
    assert_eq!(FileName::from_static("stress.cfg").as_str(), "stress.cfg");
}

#[test]
#[should_panic(expected = "invalid file name literal")]
fn invalid_static_name_panics() {
    let _ = FileName::from_static("a/b");
}

#[test]
fn file_name_views_agree() {
    let owned: FileName = "results.txt".parse().unwrap();
    assert_eq!(owned.as_str(), "results.txt");
    assert_eq!(owned.to_string(), "results.txt");
    assert_eq!(AsRef::<Path>::as_ref(&owned), Path::new("results.txt"));
    assert_eq!(
        AsRef::<std::ffi::OsStr>::as_ref(&owned),
        std::ffi::OsStr::new("results.txt")
    );
    assert_eq!(PLAIN, "prime.txt".parse().unwrap());
}

#[test]
fn real_fs_executable_regular_file_passes() {
    let dir = TempDir::new().unwrap();
    for mode in [0o755, 0o700, 0o100, 0o010, 0o001] {
        let path = file_with_mode(&dir, &format!("bin-{mode:o}"), mode);
        check_executable("mprime", &path).unwrap();
    }
}

#[test]
fn real_fs_file_without_execute_bit_is_refused() {
    let dir = TempDir::new().unwrap();
    let path = file_with_mode(&dir, "bin", 0o644);
    match check_executable("mprime", &path).unwrap_err() {
        Error::NotExecutable { name, path: named } => {
            assert_eq!(name, "mprime");
            assert_eq!(named, path);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn real_fs_directory_is_not_an_executable() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bin");
    fs::create_dir(&path).unwrap();
    assert!(matches!(
        check_executable("y-cruncher", &path),
        Err(Error::NotExecutable { .. })
    ));
}

#[test]
fn real_fs_symlink_to_an_executable_passes() {
    let dir = TempDir::new().unwrap();
    let target = file_with_mode(&dir, "real", 0o755);
    let link = dir.path().join("link");
    symlink(&target, &link).unwrap();
    check_executable("mprime", &link).unwrap();
}

#[test]
fn real_fs_symlink_to_a_plain_file_is_refused() {
    let dir = TempDir::new().unwrap();
    let target = file_with_mode(&dir, "real", 0o644);
    let link = dir.path().join("link");
    symlink(&target, &link).unwrap();
    assert!(matches!(
        check_executable("mprime", &link),
        Err(Error::NotExecutable { .. })
    ));
}

#[test]
fn real_fs_absent_binary_is_missing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("absent");
    let error = check_executable("mprime", &path).unwrap_err();
    assert!(error.is_missing());
    assert!(error.to_string().contains("mprime"));
    assert!(error.to_string().contains(&path.display().to_string()));
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn real_fs_dangling_symlink_is_missing() {
    let dir = TempDir::new().unwrap();
    let link = dir.path().join("link");
    symlink("absent", &link).unwrap();
    assert!(check_executable("mprime", &link).unwrap_err().is_missing());
}

#[test]
fn real_fs_cyclic_symlink_is_a_stat_error_not_missing() {
    let dir = TempDir::new().unwrap();
    let link = dir.path().join("loop");
    symlink("loop", &link).unwrap();
    let error = check_executable("mprime", &link).unwrap_err();
    assert!(matches!(error, Error::Stat { .. }));
    assert!(!error.is_missing());
    assert!(error.to_string().contains(&link.display().to_string()));
}

#[test]
fn real_fs_file_as_a_directory_component_is_a_stat_error_not_missing() {
    let dir = TempDir::new().unwrap();
    let file = file_with_mode(&dir, "file", 0o755);
    let error = check_executable("mprime", &file.join("bin")).unwrap_err();
    assert!(matches!(error, Error::Stat { .. }));
    assert!(!error.is_missing());
}

#[test]
fn only_absence_maps_to_backend_missing() {
    let root = PathBuf::from("/pkg");
    let missing = [
        Error::Missing {
            name: "mprime",
            path: root.clone(),
            source: io::ErrorKind::NotFound.into(),
        },
        Error::BinariesMissing {
            name: "y-cruncher",
            root: root.clone(),
            source: io::ErrorKind::NotFound.into(),
        },
        Error::NoBinaries {
            name: "y-cruncher",
            root: root.clone(),
        },
        Error::NoZen5 { root: root.clone() },
    ];
    for error in missing {
        assert!(error.is_missing(), "{error}");
        assert!(matches!(
            togi_machine::Error::from(error),
            togi_machine::Error::BackendMissing
        ));
    }
    let present = [
        Error::Stat {
            name: "mprime",
            path: root.clone(),
            source: io::ErrorKind::PermissionDenied.into(),
        },
        Error::NotExecutable {
            name: "mprime",
            path: root.clone(),
        },
        Error::ReadBinaries {
            name: "y-cruncher",
            root,
            source: io::ErrorKind::PermissionDenied.into(),
        },
        Error::Unsupported {
            backend: togi_machine::Backend::Mprime,
            workload: catalog("mprime-sse-4k-21k"),
        },
        Error::NoCpus {
            backend: togi_machine::Backend::Mprime,
            workload: catalog("mprime-sse-4k-21k"),
        },
        Error::Pattern {
            name: "mprime",
            source: regex::Regex::new(&String::from("(")).unwrap_err(),
        },
    ];
    for error in present {
        assert!(!error.is_missing(), "{error}");
        assert!(matches!(
            togi_machine::Error::from(error),
            togi_machine::Error::Other(_)
        ));
    }
}

#[test]
fn unsupported_workload_error_names_it() {
    let error = Error::Unsupported {
        backend: togi_machine::Backend::Ycruncher,
        workload: catalog("mprime-sse-4k-21k-smt"),
    };
    assert_eq!(
        error.to_string(),
        "ycruncher: no configuration for workload mprime-sse-4k-21k-smt"
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "negative control: backends never spawn; the inventory must reject this call"
)]
fn spawn_control() -> io::Result<std::process::ExitStatus> {
    std::process::Command::new("true").status()
}

#[test]
fn spawn_control_is_referenced_never_run() {
    std::hint::black_box(spawn_control);
}
