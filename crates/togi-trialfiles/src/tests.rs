use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};
use std::path::Path;

use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use nix::sys::stat::Mode;
use nix::unistd::mkfifo;
use tempfile::TempDir;
use test_case::test_case;

use super::{Error, SAMPLES, compress, compress_file, open, sync_dir};

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

fn gzip_bytes(content: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(content).unwrap();
    encoder.finish().unwrap()
}

/// Several hundred KiB of distinct lines, far beyond the 32 KiB DEFLATE window.
fn large_log() -> Vec<u8> {
    let mut log = String::new();
    for i in 0_u64..20_000 {
        writeln!(
            log,
            "{{\"line\":{i},\"value\":{}}}",
            i * i * 7919 % 1_000_003
        )
        .unwrap();
    }
    log.into_bytes()
}

/// Reads the trial file chunk by chunk, keeping what was decoded before the stream ended or failed.
fn read_prefix(path: &Path) -> (Vec<u8>, io::Result<()>) {
    let mut reader = open(path).unwrap();
    let mut data = Vec::new();
    let mut buf = [0; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return (data, Ok(())),
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return (data, Err(error)),
        }
    }
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

fn injected() -> io::Error {
    io::Error::other("injected sync failure")
}

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
    let errors = compress(dir.path()).unwrap_err();
    assert!(
        matches!(errors.errors(), [Error::Compress { path, .. }] if path == &dir.path().join(SAMPLES)),
        "{errors:?}"
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
        Err(injected())
    })
    .unwrap_err();
    assert!(
        matches!(&error, Error::SyncCompressed { .. }),
        "compression with failed directory sync = {error:?}"
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
    let errors = compress(dir.path()).unwrap_err();
    let failed: Vec<&Path> = errors
        .errors()
        .iter()
        .map(|error| match error {
            Error::Compress { path, .. } => path.as_path(),
            other => panic!("unexpected error {other:?}"),
        })
        .collect();
    assert_eq!(
        failed,
        [
            dir.path().join("a/stdout.log"),
            dir.path().join("b/stderr.log")
        ]
    );
    assert_eq!(errors.to_string().lines().count(), 2);
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
    assert!(matches!(errors.errors(), [Error::List { dir, .. }] if dir == &file));
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
    read_trial_file(&dir.path().join("log")).unwrap_err();
    write(&dir.path().join("log.gz"), "not gzip at all");
    read_trial_file(&dir.path().join("log")).unwrap_err();
}

#[test]
fn compressed_log_larger_than_the_deflate_window_round_trips() {
    let dir = TempDir::new().unwrap();
    let log = large_log();
    assert!(log.len() > 256 * 1024);
    fs::write(dir.path().join(SAMPLES), &log).unwrap();
    compress(dir.path()).unwrap();
    assert_eq!(entries(dir.path()), [format!("{SAMPLES}.gz")]);
    assert_eq!(read_trial_file(&dir.path().join(SAMPLES)).unwrap(), log);
}

#[test]
fn gzip_truncated_inside_the_body_yields_a_prefix_then_an_error() {
    let dir = TempDir::new().unwrap();
    let log = large_log();
    let gz = gzip_bytes(&log);
    fs::write(dir.path().join("log.gz"), &gz[..gz.len() / 2]).unwrap();
    let (prefix, end) = read_prefix(&dir.path().join("log"));
    assert!(!prefix.is_empty());
    assert!(prefix.len() < log.len());
    assert!(log.starts_with(&prefix));
    assert_eq!(end.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn corrupt_deflate_body_is_an_error() {
    let dir = TempDir::new().unwrap();
    let mut gz = gzip_bytes(b"one\ntwo\n");
    // A fixed gzip header is 10 bytes; 0xFF there starts a final block of the reserved type 3.
    gz[10] = 0xFF;
    fs::write(dir.path().join("log.gz"), gz).unwrap();
    let (_, end) = read_prefix(&dir.path().join("log"));
    assert_eq!(end.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test_case(8; "crc mismatch")]
#[test_case(4; "isize mismatch")]
fn gzip_trailer_mismatch_is_an_error(from_end: usize) {
    let dir = TempDir::new().unwrap();
    let mut gz = gzip_bytes(b"one\ntwo\n");
    let at = gz.len() - from_end;
    gz[at] ^= 0xFF;
    fs::write(dir.path().join("log.gz"), gz).unwrap();
    let (_, end) = read_prefix(&dir.path().join("log"));
    assert_eq!(end.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn trailing_data_after_a_gzip_member_is_an_error() {
    let dir = TempDir::new().unwrap();
    let mut gz = gzip_bytes(b"one\ntwo\n");
    gz.extend_from_slice(b"trailing garbage");
    fs::write(dir.path().join("log.gz"), gz).unwrap();
    let (prefix, end) = read_prefix(&dir.path().join("log"));
    assert_eq!(prefix, b"one\ntwo\n");
    assert_eq!(end.unwrap_err().kind(), io::ErrorKind::InvalidInput);
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
        matches!(&error, Error::RemoveStale { path, .. } if path == &stale),
        "{error:?}"
    );
    assert_eq!(entries(dir.path()), ["log", "log.gz.tmp"]);
}

#[test]
fn empty_stale_temporary_directory_is_refused_not_removed() {
    let dir = TempDir::new().unwrap();
    write(&dir.path().join(SAMPLES), SAMPLE_LINE);
    let stale = dir.path().join(format!("{SAMPLES}.gz.tmp"));
    fs::create_dir(&stale).unwrap();
    let errors = compress(dir.path()).unwrap_err();
    assert!(
        matches!(errors.errors(), [Error::RemoveStale { path, .. }] if path == &stale),
        "{errors:?}"
    );
    assert_eq!(
        entries(dir.path()),
        [SAMPLES.to_owned(), format!("{SAMPLES}.gz.tmp")]
    );
    let data = read_trial_file(&dir.path().join(SAMPLES)).unwrap();
    assert_eq!(String::from_utf8(data).unwrap(), SAMPLE_LINE);
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
fn unreadable_source_reports_open_error() {
    let dir = TempDir::new().unwrap();
    if !permissions_enforced(dir.path()) {
        return;
    }
    let path = dir.path().join(SAMPLES);
    write(&path, SAMPLE_LINE);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let errors = compress(dir.path()).unwrap_err();
    assert!(
        matches!(errors.errors(), [Error::Open { source, .. }] if source.kind() == io::ErrorKind::PermissionDenied),
        "{errors:?}"
    );
}

#[test]
fn unwritable_directory_reports_create_error() {
    let dir = TempDir::new().unwrap();
    if !permissions_enforced(dir.path()) {
        return;
    }
    write(&dir.path().join(SAMPLES), SAMPLE_LINE);
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let result = compress(dir.path());
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let errors = result.unwrap_err();
    assert!(
        matches!(errors.errors(), [Error::Create { source, .. }] if source.kind() == io::ErrorKind::PermissionDenied),
        "{errors:?}"
    );
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
            return Err(injected());
        }
        sync_dir(synced)
    })
    .unwrap_err();
    assert!(matches!(&error, Error::SyncRemoval { .. }), "{error:?}");
    assert_eq!(entries(dir.path()), ["log.gz"]);
    assert_eq!(read_trial_file(&path).unwrap(), b"plain\n");
}
