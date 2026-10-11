use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use nix::sys::stat::Mode;
use nix::unistd::mkfifo;
use rustix::io::Errno;
use tempfile::TempDir;
use test_case::test_case;

use super::goerr;
use super::{Cause, Error, PathError, SAMPLES, compress, compress_file, open, sync_dir};

const SAMPLE_LINE: &str = "{\"elapsed_ms\":1000}\n";

fn read_trial_file(path: &Path) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    open(path)?.read_to_end(&mut data)?;
    Ok(data)
}

fn write_gzip(path: &Path, content: &str) {
    let mut encoder = GzEncoder::new(fs::File::create(path).unwrap(), Compression::default());
    encoder.write_all(content.as_bytes()).unwrap();
    encoder.finish().unwrap().sync_all().unwrap();
}

fn gunzip(path: &Path) -> io::Result<String> {
    let mut text = String::new();
    MultiGzDecoder::new(fs::File::open(path)?).read_to_string(&mut text)?;
    Ok(text)
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn write(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
}

/// The directory as Go renders it in an error message.
fn shown(dir: &TempDir) -> String {
    goerr::clean(dir.path()).display().to_string()
}

fn sync_error(dir: &Path, errno: Errno) -> Cause {
    PathError::from_errno("sync", dir, errno.raw_os_error()).into()
}

/// An errno Go's tables do not name, so Go renders it as `errno N`.
const UNKNOWN_ERRNO: i32 = 4242;

#[test_case(|dir| write(&dir.join("log"), "one\ntwo\n"), &["log.gz"], "one\ntwo\n"; "plain becomes compressed")]
#[test_case(|_| {}, &[], ""; "missing file is skipped")]
#[test_case(|dir| write_gzip(&dir.join("log.gz"), "one\ntwo\n"), &["log.gz"], "one\ntwo\n"; "already compressed is skipped")]
#[test_case(
    |dir| {
        write(&dir.join("log"), "one\ntwo\n");
        write(&dir.join("log.gz.tmp"), "torn");
    },
    &["log.gz"], "one\ntwo\n";
    "crash before rename leaves a stale temporary"
)]
#[test_case(
    |dir| {
        write(&dir.join("log"), "one\ntwo\n");
        write_gzip(&dir.join("log.gz"), "one\ntwo\n");
    },
    &["log.gz"], "one\ntwo\n";
    "crash after rename leaves both forms"
)]
#[test_case(
    |dir| {
        write(&dir.join("log"), "one\ntwo\n");
        write(&dir.join("log.gz"), "torn");
    },
    &["log.gz"], "one\ntwo\n";
    "torn compressed file beside the plain one is replaced"
)]
#[test_case(
    |dir| {
        write(&dir.join("victim"), "protected\n");
        symlink("victim", dir.join("log")).unwrap();
    },
    &["log", "victim"], "";
    "symlink is not followed"
)]
fn compress_trial_file(setup: fn(&Path), want: &[&str], read: &str) {
    let dir = TempDir::new().unwrap();
    setup(dir.path());
    compress_file(&dir.path().join("log"), sync_dir).unwrap();
    assert_eq!(entries(dir.path()), want);
    if !read.is_empty() {
        let got = read_trial_file(&dir.path().join("log")).unwrap();
        assert_eq!(String::from_utf8(got).unwrap(), read);
    }
}

fn make_fifo(path: &Path) {
    mkfifo(path, Mode::from_bits_truncate(0o600)).unwrap();
}

fn make_dir(path: &Path) {
    fs::create_dir(path).unwrap();
}

#[test_case(make_fifo, |kind| kind.is_fifo(); "fifo")]
#[test_case(make_dir, |kind| kind.is_dir(); "directory")]
fn compress_skips_non_regular_sources(create: fn(&Path), is_expected: fn(fs::FileType) -> bool) {
    let dir = TempDir::new().unwrap();
    let instance = dir.path().join("work");
    fs::create_dir(&instance).unwrap();
    let path = instance.join("stdout.log");
    create(&path);
    compress(dir.path()).unwrap();
    assert!(is_expected(
        fs::symlink_metadata(&path).unwrap().file_type()
    ));
    assert_eq!(entries(&instance), ["stdout.log"]);
}

#[test]
fn compression_failure_retains_readable_plain_file() {
    let dir = TempDir::new().unwrap();
    write(&dir.path().join(SAMPLES), SAMPLE_LINE);
    let blocker = dir.path().join(format!("{SAMPLES}.gz"));
    fs::create_dir(&blocker).unwrap();
    let root = shown(&dir);
    let errors = compress(dir.path()).unwrap_err();
    assert_eq!(
        errors.to_string(),
        format!(
            "compress {SAMPLES}: rename {root}/{SAMPLES}.gz.tmp {root}/{SAMPLES}.gz: file exists"
        )
    );
    let data = read_trial_file(&dir.path().join(SAMPLES)).unwrap();
    assert_eq!(String::from_utf8(data).unwrap(), SAMPLE_LINE);
    assert!(
        !dir.path().join(format!("{SAMPLES}.gz.tmp")).exists(),
        "failed compression left a temporary file"
    );
    fs::remove_dir(&blocker).unwrap();
    compress(dir.path()).unwrap();
    let data = read_trial_file(&dir.path().join(SAMPLES)).unwrap();
    assert_eq!(String::from_utf8(data).unwrap(), SAMPLE_LINE);
}

#[test]
fn compression_sync_failure_after_publication_retains_plain_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(SAMPLES);
    write(&path, SAMPLE_LINE);
    let gz = dir.path().join(format!("{SAMPLES}.gz"));
    let mut syncs = 0;
    let error = compress_file(&path, |synced| {
        syncs += 1;
        assert_eq!(syncs, 1, "directory synced again after a failed sync");
        assert_eq!(synced, dir.path());
        assert_eq!(fs::read_to_string(&path).unwrap(), SAMPLE_LINE);
        assert_eq!(gunzip(&gz).unwrap(), SAMPLE_LINE);
        Err(sync_error(synced, Errno::IO))
    })
    .unwrap_err();
    assert!(
        matches!(&error, Error::SyncCompressed { .. }),
        "compression with failed directory sync = {error:?}"
    );
    assert_eq!(
        error.to_string(),
        format!(
            "sync compressed {SAMPLES}: sync {}: input/output error",
            shown(&dir)
        )
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), SAMPLE_LINE);
    let data = read_trial_file(&path).unwrap();
    assert_eq!(String::from_utf8(data).unwrap(), SAMPLE_LINE);

    let mut states = Vec::new();
    compress_file(&path, |synced| {
        states.push(entries(synced));
        sync_dir(synced)
    })
    .unwrap();
    assert_eq!(
        states,
        [
            vec![SAMPLES.to_owned(), format!("{SAMPLES}.gz")],
            vec![format!("{SAMPLES}.gz")]
        ]
    );
    assert_eq!(entries(dir.path()), [format!("{SAMPLES}.gz")]);
    assert_eq!(gunzip(&gz).unwrap(), SAMPLE_LINE);
}

#[test]
fn compression_ignores_symlink_instance_directories() {
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    write(&outside.path().join("stdout.log"), "outside\n");
    symlink(outside.path(), dir.path().join("work")).unwrap();
    compress(dir.path()).unwrap();
    assert_eq!(
        fs::read_to_string(outside.path().join("stdout.log")).unwrap(),
        "outside\n"
    );
    assert_eq!(entries(outside.path()), ["stdout.log"]);
    compress(&dir.path().join("missing")).unwrap();
}

#[test]
fn compress_covers_samples_and_every_instance_log() {
    let dir = TempDir::new().unwrap();
    write(&dir.path().join(SAMPLES), SAMPLE_LINE);
    for instance in ["a", "b"] {
        fs::create_dir(dir.path().join(instance)).unwrap();
        write(&dir.path().join(instance).join("stdout.log"), "out\n");
        write(&dir.path().join(instance).join("stderr.log"), "err\n");
        write(&dir.path().join(instance).join("other.log"), "kept\n");
    }
    write(
        &dir.path().join("stdout.log"),
        "top-level log is not a trial log\n",
    );
    compress(dir.path()).unwrap();
    assert_eq!(
        entries(dir.path()),
        ["a", "b", "samples.jsonl.gz", "stdout.log"]
    );
    for instance in ["a", "b"] {
        assert_eq!(
            entries(&dir.path().join(instance)),
            ["other.log", "stderr.log.gz", "stdout.log.gz"]
        );
        let data = read_trial_file(&dir.path().join(instance).join("stderr.log")).unwrap();
        assert_eq!(data, b"err\n");
    }
}

#[test]
fn compress_reports_each_failed_file_one_per_line_in_name_order() {
    let dir = TempDir::new().unwrap();
    for instance in ["b", "a"] {
        fs::create_dir(dir.path().join(instance)).unwrap();
        write(&dir.path().join(instance).join("stdout.log"), "o\n");
        write(&dir.path().join(instance).join("stderr.log"), "e\n");
    }
    fs::create_dir(dir.path().join("a/stdout.log.gz")).unwrap();
    fs::create_dir(dir.path().join("b/stderr.log.gz")).unwrap();
    let root = shown(&dir);
    let errors = compress(dir.path()).unwrap_err();
    assert_eq!(
        errors.to_string(),
        format!(
            "compress stdout.log: rename {root}/a/stdout.log.gz.tmp {root}/a/stdout.log.gz: file exists\n\
             compress stderr.log: rename {root}/b/stderr.log.gz.tmp {root}/b/stderr.log.gz: file exists"
        )
    );
    assert!(matches!(
        errors.errors(),
        [Error::Compress { .. }, Error::Compress { .. }]
    ));
    assert_eq!(
        entries(&dir.path().join("a")),
        ["stderr.log.gz", "stdout.log", "stdout.log.gz"]
    );
    assert_eq!(
        entries(&dir.path().join("b")),
        ["stderr.log", "stderr.log.gz", "stdout.log.gz"]
    );
}

#[test]
fn compress_of_a_file_reports_the_listing_failure() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("not-a-dir");
    write(&file, "x");
    let errors = compress(&file).unwrap_err();
    assert!(matches!(errors.errors(), [Error::List(_)]));
    assert_eq!(
        errors.to_string(),
        format!("list trial files: open {}: not a directory", file.display())
    );
}

/// Go keeps a workload-chosen directory name's raw bytes in the error; the journal's JSON encoding then writes U+FFFD
/// for each invalid byte. Linux names are bytes; Apple file systems refuse a name that is not UTF-8.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_instance_directory_keeps_go_raw_bytes_and_displays_one_replacement_per_byte() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = TempDir::new().unwrap();
    let instance = dir.path().join(OsStr::from_bytes(b"\xe2\x82"));
    fs::create_dir(&instance).unwrap();
    write(&instance.join("stdout.log"), "o\n");
    fs::create_dir(instance.join("stdout.log.gz")).unwrap();
    let root = shown(&dir);
    let errors = compress(dir.path()).unwrap_err();
    assert_eq!(
        errors.to_string(),
        format!(
            "compress stdout.log: rename {root}/\u{FFFD}\u{FFFD}/stdout.log.gz.tmp {root}/\u{FFFD}\u{FFFD}/stdout.log.gz: file exists"
        )
    );
    // Go's err.Error() keeps the raw bytes; its JSON encoder turns each into the escape \ufffd.
    let mut want = format!("compress stdout.log: rename {root}/").into_bytes();
    want.extend(b"\xe2\x82/stdout.log.gz.tmp ");
    want.extend(format!("{root}/").into_bytes());
    want.extend(b"\xe2\x82/stdout.log.gz: file exists");
    assert_eq!(errors.go_text(), want);
    assert_eq!(errors.errors()[0].go_text(), want);

    // A directory that really is named with two U+FFFD characters has the same Display text but other bytes: Go's
    // JSON encoder writes those characters as UTF-8, not as an escape.
    let real = TempDir::new().unwrap();
    let instance = real.path().join("\u{FFFD}\u{FFFD}");
    fs::create_dir(&instance).unwrap();
    write(&instance.join("stdout.log"), "o\n");
    fs::create_dir(instance.join("stdout.log.gz")).unwrap();
    let real_errors = compress(real.path()).unwrap_err();
    let real_root = shown(&real);
    let mut real_want = format!("compress stdout.log: rename {real_root}/").into_bytes();
    real_want.extend("\u{FFFD}\u{FFFD}/stdout.log.gz.tmp ".as_bytes());
    real_want.extend(format!("{real_root}/").into_bytes());
    real_want.extend("\u{FFFD}\u{FFFD}/stdout.log.gz: file exists".as_bytes());
    assert_eq!(real_errors.go_text(), real_want);
    assert_eq!(
        real_errors.to_string().replace(&real_root, "$DIR"),
        errors.to_string().replace(&root, "$DIR")
    );
    assert!(real_want.windows(3).any(|w| w == "\u{FFFD}".as_bytes()));
    assert!(
        !errors
            .go_text()
            .windows(3)
            .any(|w| w == "\u{FFFD}".as_bytes())
    );
}

#[test]
fn go_text_joins_errors_with_newlines() {
    let dir = TempDir::new().unwrap();
    for instance in ["a", "b"] {
        fs::create_dir(dir.path().join(instance)).unwrap();
        write(&dir.path().join(instance).join("stdout.log"), "o\n");
        fs::create_dir(dir.path().join(instance).join("stdout.log.gz")).unwrap();
    }
    let errors = compress(dir.path()).unwrap_err();
    assert_eq!(errors.go_text(), errors.to_string().into_bytes());
    assert_eq!(errors.go_text().split(|&b| b == b'\n').count(), 2);
}

#[test_case(b"plain", "plain"; "valid text is unchanged")]
#[test_case("caf\u{e9}".as_bytes(), "caf\u{e9}"; "valid multi-byte text is unchanged")]
#[test_case(b"\xe2\x82", "\u{FFFD}\u{FFFD}"; "truncated sequence is one replacement per byte")]
#[test_case(b"a\xed\xa0\x80b\xff", "a\u{FFFD}\u{FFFD}\u{FFFD}b\u{FFFD}"; "surrogate and stray byte")]
fn journal_text_replaces_each_invalid_byte(bytes: &[u8], want: &str) {
    assert_eq!(goerr::journal_text(bytes), want);
}

/// std rejects a path with an embedded NUL before any system call, where Go's `BytePtrFromString` reports `EINVAL`.
#[test]
fn embedded_nul_in_the_directory_reports_go_invalid_argument() {
    let dir = TempDir::new().unwrap();
    let root = shown(&dir);
    let errors = compress(&dir.path().join("a\0b")).unwrap_err();
    assert!(matches!(errors.errors(), [Error::List(_)]));
    assert_eq!(
        errors.to_string(),
        format!("list trial files: open {root}/a\0b: invalid argument")
    );
}

#[test]
fn open_prefers_the_plain_file_and_falls_back_to_gzip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("log");
    write_gzip(&dir.path().join("log.gz"), "compressed\n");
    assert_eq!(read_trial_file(&path).unwrap(), b"compressed\n");
    write(&path, "plain\n");
    assert_eq!(read_trial_file(&path).unwrap(), b"plain\n");
}

#[test]
fn open_reads_concatenated_gzip_members() {
    let dir = TempDir::new().unwrap();
    let (first, second) = (dir.path().join("one.gz"), dir.path().join("two.gz"));
    write_gzip(&first, "first\n");
    write_gzip(&second, "second\n");
    let mut joined = fs::read(&first).unwrap();
    joined.extend(fs::read(&second).unwrap());
    fs::write(dir.path().join("log.gz"), joined).unwrap();
    assert_eq!(
        read_trial_file(&dir.path().join("log")).unwrap(),
        b"first\nsecond\n"
    );
}

#[test]
fn open_reports_missing_and_corrupt_files() {
    let dir = TempDir::new().unwrap();
    let missing = open(&dir.path().join("log")).unwrap_err();
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    write(&dir.path().join("log.gz"), "torn");
    let torn = open(&dir.path().join("log")).unwrap_err();
    assert_eq!(torn.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(torn.to_string(), "unexpected EOF");
    write(&dir.path().join("log.gz"), "not gzip at all");
    let bad = open(&dir.path().join("log")).unwrap_err();
    assert_eq!(bad.kind(), io::ErrorKind::InvalidData);
    assert_eq!(bad.to_string(), "gzip: invalid header");
}

#[test]
fn unremovable_stale_temporary_fails_before_anything_changes() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("log");
    write(&path, "plain\n");
    let stale = dir.path().join("log.gz.tmp");
    fs::create_dir(&stale).unwrap();
    write(&stale.join("keep"), "x");
    let error = compress_file(&path, sync_dir).unwrap_err();
    assert!(
        matches!(&error, Error::RemoveStale { name, .. } if name == "log.gz.tmp"),
        "{error:?}"
    );
    let root = shown(&dir);
    assert_eq!(
        error.to_string(),
        format!("remove stale log.gz.tmp: remove {root}/log.gz.tmp: directory not empty")
    );
    assert_eq!(entries(dir.path()), ["log", "log.gz.tmp"]);
}

#[test]
fn empty_stale_temporary_directory_is_removed_like_go_does() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("log");
    write(&path, "plain\n");
    fs::create_dir(dir.path().join("log.gz.tmp")).unwrap();
    compress_file(&path, sync_dir).unwrap();
    assert_eq!(entries(dir.path()), ["log.gz"]);
    assert_eq!(gunzip(&dir.path().join("log.gz")).unwrap(), "plain\n");
}

#[test]
fn error_paths_are_cleaned_like_filepath_join() {
    let dir = TempDir::new().unwrap();
    write(&dir.path().join(SAMPLES), SAMPLE_LINE);
    fs::create_dir(dir.path().join(format!("{SAMPLES}.gz"))).unwrap();
    let root = shown(&dir);
    let unclean = PathBuf::from(format!("{}//./", dir.path().display()));
    let errors = compress(&unclean).unwrap_err();
    assert_eq!(
        errors.to_string(),
        format!(
            "compress {SAMPLES}: rename {root}/{SAMPLES}.gz.tmp {root}/{SAMPLES}.gz: file exists"
        )
    );
}

#[test_case("a//b/./c/../d", "a/b/d", "d", "a/b"; "interior elements")]
#[test_case("/../x", "/x", "x", "/"; "parent of root")]
#[test_case("../x", "../x", "x", ".."; "leading parent")]
#[test_case("x", "x", "x", "."; "bare name")]
#[test_case("./", ".", ".", "."; "current directory")]
fn go_path_helpers(path: &str, cleaned: &str, base: &str, dirname: &str) {
    assert_eq!(goerr::clean(Path::new(path)), Path::new(cleaned));
    assert_eq!(goerr::base(Path::new(cleaned)), base);
    assert_eq!(goerr::dir(Path::new(cleaned)), Path::new(dirname));
}

/// Whether permission bits are enforced; a privileged user reads a mode 000 file and writes a read-only directory.
fn permissions_enforced(dir: &Path) -> bool {
    let probe = dir.join("probe");
    write(&probe, "x");
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).unwrap();
    let enforced = fs::File::open(&probe).is_err();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&probe).unwrap();
    enforced
}

#[test]
fn unreadable_source_reports_go_open_error() {
    let dir = TempDir::new().unwrap();
    if !permissions_enforced(dir.path()) {
        return;
    }
    let path = dir.path().join(SAMPLES);
    write(&path, SAMPLE_LINE);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let errors = compress(dir.path()).unwrap_err();
    assert_eq!(
        errors.to_string(),
        format!(
            "open {SAMPLES}: open {}/{SAMPLES}: permission denied",
            shown(&dir)
        )
    );
    assert!(matches!(errors.errors(), [Error::Open { .. }]));
}

#[test]
fn unwritable_directory_reports_go_create_error() {
    let dir = TempDir::new().unwrap();
    if !permissions_enforced(dir.path()) {
        return;
    }
    write(&dir.path().join(SAMPLES), SAMPLE_LINE);
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let result = compress(dir.path());
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let errors = result.unwrap_err();
    assert_eq!(
        errors.to_string(),
        format!(
            "create {SAMPLES}.gz.tmp: open {}/{SAMPLES}.gz.tmp: permission denied",
            shown(&dir)
        )
    );
    assert!(matches!(errors.errors(), [Error::Create { .. }]));
}

#[test]
fn sync_failure_after_removal_leaves_only_the_compressed_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("log");
    write(&path, "plain\n");
    let mut syncs = 0;
    let error = compress_file(&path, |synced| {
        syncs += 1;
        if syncs == 2 {
            return Err(PathError::from_errno("sync", synced, UNKNOWN_ERRNO).into());
        }
        sync_dir(synced)
    })
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("sync removal of log: sync {}: errno 4242", shown(&dir))
    );
    assert_eq!(entries(dir.path()), ["log.gz"]);
    assert_eq!(read_trial_file(&path).unwrap(), b"plain\n");
}
