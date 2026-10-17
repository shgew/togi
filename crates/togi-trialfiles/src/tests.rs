use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nix::sys::stat::Mode;
use nix::unistd::mkfifo;
use tempfile::TempDir;
use test_case::test_case;

use super::{Disk, Error, SAMPLES, Storage, compress_file, compress_with, open, read_gzip};

const SAMPLE_LINE: &str = "{\"elapsed_ms\":1000}\n";

fn gzip_bytes(content: &[u8]) -> Vec<u8> {
    super::write_gzip(&mut Cursor::new(content), Vec::new()).unwrap()
}

fn decoded(content: &[u8]) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    read_gzip(content).read_to_end(&mut data)?;
    Ok(data)
}

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

fn read_prefix(content: &[u8]) -> (Vec<u8>, io::Result<()>) {
    let mut reader = read_gzip(content);
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

#[derive(Default)]
struct MemoryState {
    files: BTreeMap<PathBuf, Vec<u8>>,
    stable: BTreeMap<PathBuf, Vec<u8>>,
    events: Vec<&'static str>,
    fail: Option<&'static str>,
    directory_syncs: usize,
}

struct MemoryFile {
    path: PathBuf,
    state: Rc<RefCell<MemoryState>>,
}

impl Write for MemoryFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.state.borrow_mut();
        state.events.push("write");
        if state.fail == Some("write") {
            return Err(io::Error::other("injected write failure"));
        }
        state
            .files
            .get_mut(&self.path)
            .unwrap()
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Memory {
    state: Rc<RefCell<MemoryState>>,
    instances: Vec<PathBuf>,
}

impl Memory {
    fn put(&mut self, path: &str, content: &[u8]) {
        let mut state = self.state.borrow_mut();
        state.files.insert(PathBuf::from(path), content.to_vec());
        state.stable = state.files.clone();
    }

    fn step(&self, event: &'static str) -> io::Result<()> {
        let mut state = self.state.borrow_mut();
        state.events.push(event);
        if state.fail == Some(event) {
            return Err(io::Error::other(format!("injected {event} failure")));
        }
        Ok(())
    }

    fn read(&self, path: &str, after_crash: bool) -> io::Result<Vec<u8>> {
        let state = self.state.borrow();
        let files = if after_crash {
            &state.stable
        } else {
            &state.files
        };
        if let Some(plain) = files.get(Path::new(path)) {
            return Ok(plain.clone());
        }
        let compressed = files
            .get(&super::with_suffix(Path::new(path), ".gz"))
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        decoded(compressed)
    }

    fn names(&self) -> Vec<String> {
        self.state
            .borrow()
            .files
            .keys()
            .map(|path| path.display().to_string())
            .collect()
    }
}

impl Storage for Memory {
    type Source = Cursor<Vec<u8>>;
    type Destination = MemoryFile;

    fn instance_dirs(&mut self, _: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(self.instances.clone())
    }

    fn open_regular(&mut self, path: &Path) -> Result<Option<Self::Source>, Error> {
        Ok(self
            .state
            .borrow()
            .files
            .get(path)
            .cloned()
            .map(Cursor::new))
    }

    fn create(&mut self, path: &Path) -> io::Result<MemoryFile> {
        self.step("create")?;
        self.state
            .borrow_mut()
            .files
            .insert(path.to_owned(), Vec::new());
        Ok(MemoryFile {
            path: path.to_owned(),
            state: Rc::clone(&self.state),
        })
    }

    fn sync_file(&mut self, file: &mut MemoryFile) -> io::Result<()> {
        self.step("sync_file")?;
        // A publication sync must never make a partial gzip stream visible.
        decoded(&self.state.borrow().files[&file.path])?;
        Ok(())
    }

    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        self.step("rename")?;
        let mut state = self.state.borrow_mut();
        let bytes = state.files.remove(from).unwrap();
        state.files.insert(to.to_owned(), bytes);
        Ok(())
    }

    fn remove(&mut self, path: &Path) -> io::Result<()> {
        let event = if path.extension().is_some_and(|ext| ext == "tmp") {
            "remove_tmp"
        } else {
            "remove_plain"
        };
        self.step(event)?;
        self.state
            .borrow_mut()
            .files
            .remove(path)
            .map(|_| ())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn sync_dir(&mut self, _: &Path) -> io::Result<()> {
        let event = {
            let mut state = self.state.borrow_mut();
            state.directory_syncs += 1;
            if state.directory_syncs == 1 {
                "sync_publication"
            } else {
                "sync_removal"
            }
        };
        self.step(event)?;
        let mut state = self.state.borrow_mut();
        state.stable = state.files.clone();
        Ok(())
    }
}

#[test_case(false, false; "plain becomes compressed")]
#[test_case(true, false; "stale temporary is replaced")]
#[test_case(false, true; "crash leaves both forms")]
#[test_case(true, true; "stale temporary and published gzip")]
fn compression_retries_complete_content(stale: bool, published: bool) {
    let mut memory = Memory::default();
    memory.put("trial/log", b"one\ntwo\n");
    if stale {
        memory.put("trial/log.gz.tmp", b"torn");
    }
    if published {
        memory.put("trial/log.gz", b"torn");
    }
    compress_file(Path::new("trial/log"), &mut memory).unwrap();
    assert_eq!(memory.names(), ["trial/log.gz"]);
    assert_eq!(memory.read("trial/log", false).unwrap(), b"one\ntwo\n");
    assert_eq!(memory.read("trial/log", true).unwrap(), b"one\ntwo\n");
}

#[test]
fn compression_skips_missing_and_already_compressed_sources() {
    let mut memory = Memory::default();
    compress_file(Path::new("trial/log"), &mut memory).unwrap();
    memory.put("trial/log.gz", &gzip_bytes(b"kept\n"));
    compress_file(Path::new("trial/log"), &mut memory).unwrap();
    assert_eq!(memory.read("trial/log", false).unwrap(), b"kept\n");
    assert!(memory.state.borrow().events.is_empty());
}

#[test]
fn compression_pins_write_sync_publish_remove_order() {
    let mut memory = Memory::default();
    memory.put("trial/log", b"one\ntwo\n");
    compress_file(Path::new("trial/log"), &mut memory).unwrap();
    let state = memory.state.borrow();
    let events: Vec<_> = state
        .events
        .iter()
        .copied()
        .filter(|event| *event != "write")
        .collect();
    assert_eq!(
        events,
        [
            "remove_tmp",
            "create",
            "sync_file",
            "rename",
            "sync_publication",
            "remove_plain",
            "sync_removal"
        ]
    );
    let first_write = state
        .events
        .iter()
        .position(|event| *event == "write")
        .unwrap();
    let file_sync = state
        .events
        .iter()
        .position(|event| *event == "sync_file")
        .unwrap();
    assert!(first_write < file_sync);
    assert!(
        state.events[file_sync..]
            .iter()
            .all(|event| *event != "write")
    );
}

#[test_case("remove_tmp"; "stale removal fails")]
#[test_case("create"; "temporary creation fails")]
fn compression_setup_failure_never_writes_or_publishes(fail: &'static str) {
    let mut memory = Memory::default();
    memory.put("trial/log", b"one\ntwo\n");
    memory.state.borrow_mut().fail = Some(fail);
    let error = compress_file(Path::new("trial/log"), &mut memory).unwrap_err();
    if fail == "create" {
        assert!(matches!(error, Error::Create { .. }));
    } else {
        assert!(matches!(error, Error::RemoveStale { .. }));
    }
    assert_eq!(memory.names(), ["trial/log"]);
    assert_eq!(memory.read("trial/log", true).unwrap(), b"one\ntwo\n");
    assert!(!memory.state.borrow().events.contains(&"write"));
    assert!(!memory.state.borrow().events.contains(&"rename"));
}

#[test_case("write"; "write fails")]
#[test_case("sync_file"; "file sync fails")]
#[test_case("rename"; "rename fails")]
#[test_case("sync_publication"; "publication sync fails")]
#[test_case("remove_plain"; "plain removal fails")]
#[test_case("sync_removal"; "removal sync fails")]
fn compression_failure_keeps_a_complete_readable_form_after_crash(fail: &'static str) {
    let mut memory = Memory::default();
    memory.put("trial/log", b"one\ntwo\n");
    memory.state.borrow_mut().fail = Some(fail);
    let error = compress_file(Path::new("trial/log"), &mut memory).unwrap_err();
    match fail {
        "write" | "sync_file" | "rename" => assert!(matches!(error, Error::Compress { .. })),
        "sync_publication" => assert!(matches!(error, Error::SyncCompressed { .. })),
        "remove_plain" => assert!(matches!(error, Error::RemovePlain { .. })),
        "sync_removal" => assert!(matches!(error, Error::SyncRemoval { .. })),
        _ => unreachable!(),
    }
    assert_eq!(memory.read("trial/log", false).unwrap(), b"one\ntwo\n");
    assert_eq!(memory.read("trial/log", true).unwrap(), b"one\ntwo\n");
    let state = memory.state.borrow();
    if matches!(fail, "write" | "sync_file" | "rename" | "sync_publication") {
        assert!(state.files.contains_key(Path::new("trial/log")));
        assert!(!state.events.contains(&"remove_plain"));
    }
    if fail == "sync_removal" {
        assert!(!state.files.contains_key(Path::new("trial/log")));
        assert!(state.stable.contains_key(Path::new("trial/log")));
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "in-memory compression tests exercise traversal through the injected adapter"
)]
#[test]
fn compress_covers_samples_and_every_instance_log() {
    let mut memory = Memory {
        instances: vec![PathBuf::from("trial/a"), PathBuf::from("trial/b")],
        ..Memory::default()
    };
    memory.put("trial/samples.jsonl", SAMPLE_LINE.as_bytes());
    for instance in ["a", "b"] {
        for log in ["stdout", "stderr", "other"] {
            memory.put(&format!("trial/{instance}/{log}.log"), b"content\n");
        }
    }
    memory.put("trial/stdout.log", b"not a trial log\n");
    compress_with(Path::new("trial"), &mut memory).unwrap();
    assert_eq!(
        memory.names(),
        [
            "trial/a/other.log",
            "trial/a/stderr.log.gz",
            "trial/a/stdout.log.gz",
            "trial/b/other.log",
            "trial/b/stderr.log.gz",
            "trial/b/stdout.log.gz",
            "trial/samples.jsonl.gz",
            "trial/stdout.log"
        ]
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "in-memory compression tests inject publication failures without touching disk"
)]
#[test]
fn compress_reports_each_failed_file_one_per_line_in_name_order() {
    let mut memory = Memory {
        instances: vec![PathBuf::from("trial/a"), PathBuf::from("trial/b")],
        ..Memory::default()
    };
    for instance in ["a", "b"] {
        for log in ["stdout", "stderr"] {
            memory.put(&format!("trial/{instance}/{log}.log"), b"content\n");
        }
    }
    memory.state.borrow_mut().fail = Some("rename");
    let errors = compress_with(Path::new("trial"), &mut memory).unwrap_err();
    let failed: Vec<_> = errors
        .errors()
        .iter()
        .map(|error| {
            let Error::Compress { path, .. } = error else {
                panic!("unexpected error {error:?}");
            };
            path.to_str().unwrap()
        })
        .collect();
    assert_eq!(
        failed,
        [
            "trial/a/stdout.log",
            "trial/a/stderr.log",
            "trial/b/stdout.log",
            "trial/b/stderr.log"
        ]
    );
    assert_eq!(errors.to_string().lines().count(), 4);
}

#[test]
fn concatenated_gzip_members_are_read() {
    let mut joined = gzip_bytes(b"first\n");
    joined.extend(gzip_bytes(b"second\n"));
    assert_eq!(decoded(&joined).unwrap(), b"first\nsecond\n");
}

#[test_case(b"torn"; "torn header")]
#[test_case(b"not gzip at all"; "invalid header")]
fn corrupt_gzip_is_rejected(content: &[u8]) {
    decoded(content).unwrap_err();
}

#[test]
fn compressed_log_larger_than_the_deflate_window_round_trips() {
    let log = large_log();
    assert!(log.len() > 256 * 1024);
    assert_eq!(decoded(&gzip_bytes(&log)).unwrap(), log);
}

#[test]
fn gzip_truncated_inside_the_body_yields_a_prefix_then_an_error() {
    let log = large_log();
    let gz = gzip_bytes(&log);
    let (prefix, end) = read_prefix(&gz[..gz.len() / 2]);
    assert!(!prefix.is_empty());
    assert!(prefix.len() < log.len());
    assert!(log.starts_with(&prefix));
    assert_eq!(end.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn corrupt_deflate_body_is_an_error() {
    let mut gz = gzip_bytes(b"one\ntwo\n");
    gz[10] = 0xFF;
    let (_, end) = read_prefix(&gz);
    assert_eq!(end.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test_case(8; "crc mismatch")]
#[test_case(4; "isize mismatch")]
fn gzip_trailer_mismatch_is_an_error(from_end: usize) {
    let mut gz = gzip_bytes(b"one\ntwo\n");
    let at = gz.len() - from_end;
    gz[at] ^= 0xFF;
    let (_, end) = read_prefix(&gz);
    assert_eq!(end.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn trailing_data_after_a_gzip_member_is_an_error() {
    let mut gz = gzip_bytes(b"one\ntwo\n");
    gz.extend_from_slice(b"trailing garbage");
    let (prefix, end) = read_prefix(&gz);
    assert_eq!(prefix, b"one\ntwo\n");
    assert_eq!(end.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[expect(
    clippy::disallowed_methods,
    reason = "kernel adapter tests exercise disposable files without requesting durability"
)]
fn compress_unsynced(dir: &Path) -> Result<(), super::Errors> {
    super::compress_unsynced(dir)
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn read_trial_file(path: &Path) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    open(path)?.read_to_end(&mut data)?;
    Ok(data)
}

#[test]
fn real_fs_open_prefers_plain_and_falls_back_to_gzip_and_reports_missing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("log");
    assert_eq!(open(&path).unwrap_err().kind(), io::ErrorKind::NotFound);
    fs::write(dir.path().join("log.gz"), gzip_bytes(b"compressed\n")).unwrap();
    assert_eq!(read_trial_file(&path).unwrap(), b"compressed\n");
    fs::write(&path, b"plain\n").unwrap();
    assert_eq!(read_trial_file(&path).unwrap(), b"plain\n");
}

fn make_fifo(path: &Path) {
    mkfifo(path, Mode::from_bits_truncate(0o600)).unwrap();
}
fn make_dir(path: &Path) {
    fs::create_dir(path).unwrap();
}

#[test_case(make_fifo, |kind| kind.is_fifo(); "fifo")]
#[test_case(make_dir, |kind| kind.is_dir(); "directory")]
fn real_fs_compress_skips_non_regular_sources(
    create: fn(&Path),
    expected: fn(fs::FileType) -> bool,
) {
    let dir = TempDir::new().unwrap();
    let instance = dir.path().join("work");
    fs::create_dir(&instance).unwrap();
    let path = instance.join("stdout.log");
    create(&path);
    compress_unsynced(dir.path()).unwrap();
    assert!(expected(fs::symlink_metadata(&path).unwrap().file_type()));
    assert_eq!(entries(&instance), ["stdout.log"]);
}

#[test]
fn real_fs_compression_ignores_symlink_sources_and_instance_directories() {
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("stdout.log"), b"outside\n").unwrap();
    symlink(outside.path(), dir.path().join("work")).unwrap();
    symlink(outside.path().join("stdout.log"), dir.path().join(SAMPLES)).unwrap();
    for name in ["b", "a"] {
        fs::create_dir(dir.path().join(name)).unwrap();
    }
    assert_eq!(
        Disk { durable: false }.instance_dirs(dir.path()).unwrap(),
        [dir.path().join("a"), dir.path().join("b")]
    );
    compress_unsynced(dir.path()).unwrap();
    assert_eq!(entries(outside.path()), ["stdout.log"]);
    assert_eq!(
        fs::read(outside.path().join("stdout.log")).unwrap(),
        b"outside\n"
    );
    assert!(
        fs::symlink_metadata(dir.path().join(SAMPLES))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    compress_unsynced(&dir.path().join("missing")).unwrap();
}

#[test]
fn real_fs_compression_publication_failure_preserves_plain_and_cleans_temporary() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(SAMPLES), SAMPLE_LINE).unwrap();
    let blocker = dir.path().join(format!("{SAMPLES}.gz"));
    fs::create_dir(&blocker).unwrap();
    let errors = compress_unsynced(dir.path()).unwrap_err();
    assert!(
        matches!(errors.errors(), [Error::Compress { .. }]),
        "{errors:?}"
    );
    assert_eq!(
        read_trial_file(&dir.path().join(SAMPLES)).unwrap(),
        SAMPLE_LINE.as_bytes()
    );
    assert!(!dir.path().join(format!("{SAMPLES}.gz.tmp")).exists());
    fs::remove_dir(&blocker).unwrap();
    compress_unsynced(dir.path()).unwrap();
    assert_eq!(entries(dir.path()), [format!("{SAMPLES}.gz")]);
    assert_eq!(
        read_trial_file(&dir.path().join(SAMPLES)).unwrap(),
        SAMPLE_LINE.as_bytes()
    );
}

#[test_case(false; "empty stale directory")]
#[test_case(true; "nonempty stale directory")]
fn real_fs_stale_temporary_directories_are_refused_not_removed(nonempty: bool) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("log");
    fs::write(&path, b"plain\n").unwrap();
    let stale = dir.path().join("log.gz.tmp");
    fs::create_dir(&stale).unwrap();
    if nonempty {
        fs::write(stale.join("keep"), b"x").unwrap();
    }
    let error = compress_file(&path, &mut Disk { durable: false }).unwrap_err();
    assert!(
        matches!(&error, Error::RemoveStale { path, .. } if path == &stale),
        "{error:?}"
    );
    assert_eq!(entries(dir.path()), ["log", "log.gz.tmp"]);
}

#[test]
fn real_fs_compress_of_a_file_reports_listing_failure() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("not-a-dir");
    fs::write(&file, b"x").unwrap();
    let errors = compress_unsynced(&file).unwrap_err();
    assert!(matches!(errors.errors(), [Error::List { dir, .. }] if dir == &file));
}

fn permissions_enforced(dir: &Path) -> bool {
    let probe = dir.join("probe");
    fs::write(&probe, b"x").unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).unwrap();
    let enforced = fs::File::open(&probe).is_err();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&probe).unwrap();
    enforced
}

#[test]
fn real_fs_unreadable_source_reports_open_error() {
    let dir = TempDir::new().unwrap();
    if !permissions_enforced(dir.path()) {
        return;
    }
    let path = dir.path().join(SAMPLES);
    fs::write(&path, SAMPLE_LINE).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let errors = compress_unsynced(dir.path()).unwrap_err();
    assert!(
        matches!(errors.errors(), [Error::Open { source, .. }] if source.kind() == io::ErrorKind::PermissionDenied),
        "{errors:?}"
    );
}

#[test]
fn real_fs_unwritable_directory_reports_create_error() {
    let dir = TempDir::new().unwrap();
    if !permissions_enforced(dir.path()) {
        return;
    }
    fs::write(dir.path().join(SAMPLES), SAMPLE_LINE).unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let result = compress_unsynced(dir.path());
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let errors = result.unwrap_err();
    assert!(
        matches!(errors.errors(), [Error::Create { source, .. }] if source.kind() == io::ErrorKind::PermissionDenied),
        "{errors:?}"
    );
}

#[test]
fn disallowed_compression_adapters_are_never_executed() {
    #[expect(
        clippy::disallowed_methods,
        reason = "pins the unsynced compression tripwire without opening files"
    )]
    fn unsynced(path: &Path) -> Result<(), super::Errors> {
        super::compress_unsynced(path)
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "pins the injected compression tripwire without doing I/O"
    )]
    fn injected(path: &Path, storage: &mut Memory) -> Result<(), super::Errors> {
        super::compress_with(path, storage)
    }
    let _ = (unsynced, injected);
}
