//! Journal notifications from inotify.
//!
//! The state directory is watched, so the journal's creation, replacement and removal are seen, and its parent
//! too, so the directory's own replacement is. Where the state directory cannot be watched (a root run leaves it
//! traverse-only) the journal itself is watched. While nothing at all can be watched for the journal, it is looked
//! for once an interval.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;
use tokio::io::unix::AsyncFd;

use super::Watch;
use crate::terminal::WatchError;

const JOURNAL: &str = "events.jsonl";

const DIRECTORY: WatchFlags = WatchFlags::MODIFY
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::CREATE)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::DELETE)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::ONLYDIR);

const FILE: WatchFlags = WatchFlags::MODIFY
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF);

fn failed(operation: &'static str, errno: Errno) -> WatchError {
    WatchError {
        operation,
        source: io::Error::from(errno),
    }
}

/// Classifies a decoded event without depending on an inotify descriptor.
fn relevant(
    watches: &HashMap<i32, PathBuf>,
    directory: &Path,
    journal: &Path,
    wd: i32,
    flags: ReadFlags,
    name: Option<&OsStr>,
) -> bool {
    if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
        return true;
    }
    let Some(watched) = watches.get(&wd) else {
        return false;
    };
    if flags.intersects(ReadFlags::IGNORED | ReadFlags::DELETE_SELF | ReadFlags::MOVE_SELF)
        || watched == journal
    {
        return true;
    }
    if watched == directory {
        return name == Some(OsStr::new(JOURNAL));
    }
    let first = directory
        .strip_prefix(watched)
        .ok()
        .and_then(|below| below.components().next());
    match (first, name) {
        (Some(first), Some(name)) => first.as_os_str() == name,
        _ => false,
    }
}

/// The inotify watches for one journal.
pub(in crate::terminal) struct Inotify {
    fd: AsyncFd<OwnedFd>,
    directory: PathBuf,
    journal: PathBuf,
    watches: HashMap<i32, PathBuf>,
    polling: bool,
}

/// Starts watching the journal in `directory`, which has to be called inside the runtime.
pub(in crate::terminal) fn open(directory: &Path) -> Result<Inotify, WatchError> {
    let directory = std::path::absolute(directory).map_err(|source| WatchError {
        operation: "resolve state directory",
        source,
    })?;
    let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)
        .map_err(|errno| failed("open journal notifications", errno))?;
    let fd = AsyncFd::new(fd).map_err(|source| WatchError {
        operation: "open journal notifications",
        source,
    })?;
    let mut watcher = Inotify {
        journal: directory.join(JOURNAL),
        directory,
        fd,
        watches: HashMap::new(),
        polling: false,
    };
    watcher.arm()?;
    Ok(watcher)
}

impl Inotify {
    /// Whether an event is about the journal: a lost queue, or an event of the journal, the state directory, or
    /// one of the directories above it that is watched for the state directory's sake.
    fn is_relevant(&self, wd: i32, flags: ReadFlags, name: Option<&OsStr>) -> bool {
        relevant(
            &self.watches,
            &self.directory,
            &self.journal,
            wd,
            flags,
            name,
        )
    }

    /// Reads every pending event and reports whether any of them matters.
    fn drain(&self) -> io::Result<bool> {
        let mut buf = [MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(self.fd.get_ref(), &mut buf);
        let mut relevant = false;
        loop {
            let event = match reader.next() {
                Ok(event) => event,
                Err(Errno::AGAIN) => return Ok(relevant),
                Err(errno) => return Err(errno.into()),
            };
            let name = event
                .file_name()
                .map(|name| OsStr::from_bytes(name.to_bytes()));
            relevant |= self.is_relevant(event.wd(), event.events(), name);
        }
    }

    /// Watches what there is to watch for the journal, adding each watch with `add`, and drops every watch it no
    /// longer needs. A directory toward the state directory made after the climb found it missing but before the
    /// directory above it was watched made no event, so then the climb starts again, at most once a level.
    fn arm_with(
        &mut self,
        mut add: impl FnMut(BorrowedFd<'_>, &Path, WatchFlags) -> Result<i32, Errno>,
    ) -> Result<(), WatchError> {
        let mut superseded = Vec::new();
        let mut climbs = self.directory.components().count();
        let climb = loop {
            let climb = self.climb(&mut add)?;
            climbs -= 1;
            match &climb.passed {
                Some(missing) if climbs > 0 && missing.is_dir() => {
                    superseded.extend(climb.watches.into_keys());
                }
                _ => break climb,
            }
        };
        let fd = self.fd.get_ref();
        for wd in self.watches.keys().chain(&superseded) {
            if !climb.watches.contains_key(wd) {
                let _ = inotify::remove_watch(fd, *wd);
            }
        }
        self.watches = climb.watches;
        self.polling = climb.polling;
        Ok(())
    }

    /// Climbs from the state directory to the first directory above it that can be watched.
    fn climb(
        &self,
        add: &mut impl FnMut(BorrowedFd<'_>, &Path, WatchFlags) -> Result<i32, Errno>,
    ) -> Result<Climb, WatchError> {
        let fd = self.fd.get_ref().as_fd();
        let mut watches = HashMap::with_capacity(2);
        let mut polling = false;
        let mut missing = None;
        let mut path = self.directory.clone();
        loop {
            match add(fd, &path, DIRECTORY) {
                Ok(wd) => {
                    watches.insert(wd, path.clone());
                    if path != self.directory {
                        break;
                    }
                }
                Err(Errno::ACCESS) if path == self.directory => {
                    match add(fd, &self.journal, FILE) {
                        Ok(wd) => {
                            watches.insert(wd, self.journal.clone());
                        }
                        Err(Errno::NOENT) => polling = true,
                        Err(errno) => return Err(failed("watch journal", errno)),
                    }
                }
                Err(Errno::ACCESS) => {
                    polling = polling || watches.is_empty();
                    missing = None;
                    break;
                }
                Err(Errno::NOENT | Errno::NOTDIR) => missing = Some(path.clone()),
                Err(errno) => return Err(failed("watch state directory", errno)),
            }
            let Some(parent) = path.parent() else {
                if polling || !watches.is_empty() {
                    break;
                }
                return Err(failed("watch state directory", Errno::NOENT));
            };
            path = parent.to_path_buf();
        }
        Ok(Climb {
            watches,
            polling,
            passed: missing,
        })
    }
}

/// What one climb watched, and the missing directory it passed last before the directory it watched above it.
struct Climb {
    watches: HashMap<i32, PathBuf>,
    polling: bool,
    passed: Option<PathBuf>,
}

impl Watch for Inotify {
    fn polling(&self) -> bool {
        self.polling
    }

    fn arm(&mut self) -> Result<(), WatchError> {
        self.arm_with(|fd, path, flags| inotify::add_watch(fd, path, flags))
    }

    async fn changed(&mut self) -> Result<(), WatchError> {
        loop {
            let mut ready = self.fd.readable().await.map_err(|source| WatchError {
                operation: "read journal notifications",
                source,
            })?;
            let relevant = self.drain().map_err(|source| WatchError {
                operation: "read journal notifications",
                source,
            })?;
            ready.clear_ready();
            if relevant {
                return self.arm();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::time::Duration;

    use rustix::fs::inotify::{ReadFlags, WatchFlags};
    use tempfile::TempDir;
    use test_case::test_case;
    use tokio::time::timeout;

    use super::{DIRECTORY, FILE, Inotify, JOURNAL, open};
    use crate::terminal::notify::Watch as _;

    /// A real wait is bounded by this; it only passes when a change that was made never arrives.
    const BOUND: Duration = Duration::from_secs(20);

    async fn arrives(watcher: &mut Inotify) {
        timeout(BOUND, watcher.changed())
            .await
            .expect("the change never arrived")
            .unwrap();
    }

    fn watcher_at(directory: &Path) -> Inotify {
        let runtime = tokio::runtime::Handle::current();
        let _guard = runtime.enter();
        open(directory).unwrap()
    }

    fn is_root() -> bool {
        rustix::process::geteuid().is_root()
    }

    fn journal_in(directory: &Path, text: &str) {
        fs::write(directory.join(JOURNAL), text).unwrap();
    }

    /// Makes a directory listable again when dropped, so that a test that fails leaves no directory behind that
    /// cannot be removed.
    struct Restore<'a>(&'a Path);

    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(0o755));
        }
    }

    #[test_case(DIRECTORY; "the directory")]
    #[test_case(FILE; "the file")]
    fn notifications_never_ask_for_the_events_of_reading_the_journal(flags: WatchFlags) {
        assert!(
            !flags.intersects(WatchFlags::OPEN | WatchFlags::ACCESS | WatchFlags::CLOSE_NOWRITE)
        );
    }

    #[test]
    fn the_directory_notification_is_only_for_directories() {
        assert!(DIRECTORY.contains(WatchFlags::ONLYDIR));
    }

    #[test]
    fn relevance_follows_the_journal_the_directory_and_what_is_above_it() {
        let directory = Path::new("/state");
        let journal = directory.join(JOURNAL);
        let state = 1;
        let parent = 2;
        let unknown = i32::MAX;
        let watches = std::collections::HashMap::from([
            (state, directory.to_path_buf()),
            (parent, Path::new("/").to_path_buf()),
        ]);
        let relevant = |wd, flags, name: Option<&OsStr>| {
            super::relevant(&watches, directory, &journal, wd, flags, name)
        };
        let watched_below = |name: &str| relevant(state, ReadFlags::CREATE, Some(OsStr::new(name)));
        assert!(watched_below(JOURNAL));
        assert!(!watched_below("events.jsonl.tmp"));
        assert!(!watched_below("state.json"));
        assert!(!relevant(state, ReadFlags::MODIFY, None));
        assert!(relevant(state, ReadFlags::DELETE_SELF, None));
        assert!(relevant(state, ReadFlags::MOVE_SELF, None));
        assert!(relevant(state, ReadFlags::IGNORED, None));
        assert!(relevant(
            parent,
            ReadFlags::CREATE,
            Some(OsStr::new("state"))
        ));
        assert!(!relevant(
            parent,
            ReadFlags::CREATE,
            Some(OsStr::new("other"))
        ));
        assert!(!relevant(parent, ReadFlags::CREATE, None));
        assert!(
            relevant(-1, ReadFlags::QUEUE_OVERFLOW, None),
            "a lost queue may have held a change of the journal"
        );
        assert!(
            !relevant(unknown, ReadFlags::DELETE_SELF, None),
            "an event of a watch that is gone"
        );
        let mut watches = watches;
        watches.insert(unknown, journal.clone());
        assert!(super::relevant(
            &watches,
            directory,
            &journal,
            unknown,
            ReadFlags::MODIFY,
            None
        ));
    }

    #[tokio::test]
    async fn real_fs_an_existing_state_directory_is_watched_with_its_parent_and_never_polled() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("state");
        fs::create_dir(&directory).unwrap();
        let watcher = watcher_at(&directory);
        assert!(!watcher.polling());
        let mut watched: Vec<_> = watcher.watches.values().cloned().collect();
        watched.sort();
        assert_eq!(watched, [root.path().to_path_buf(), directory]);
    }

    #[tokio::test]
    async fn real_fs_an_append_reaches_the_loop() {
        let root = TempDir::new().unwrap();
        journal_in(root.path(), "one\n");
        let mut watcher = watcher_at(root.path());
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(root.path().join(JOURNAL))
            .unwrap();
        std::io::Write::write_all(&mut file, b"two\n").unwrap();
        arrives(&mut watcher).await;
    }

    #[tokio::test]
    async fn real_fs_a_replacement_reaches_the_loop() {
        let root = TempDir::new().unwrap();
        journal_in(root.path(), "one\n");
        let mut watcher = watcher_at(root.path());
        fs::write(root.path().join("replacement"), "two\n").unwrap();
        fs::rename(root.path().join("replacement"), root.path().join(JOURNAL)).unwrap();
        arrives(&mut watcher).await;
    }

    #[tokio::test]
    async fn real_fs_a_removal_reaches_the_loop() {
        let root = TempDir::new().unwrap();
        journal_in(root.path(), "one\n");
        let mut watcher = watcher_at(root.path());
        fs::remove_file(root.path().join(JOURNAL)).unwrap();
        arrives(&mut watcher).await;
    }

    #[tokio::test]
    async fn real_fs_a_creation_reaches_the_loop_and_is_watched_afterwards() {
        let root = TempDir::new().unwrap();
        let mut watcher = watcher_at(root.path());
        journal_in(root.path(), "one\n");
        arrives(&mut watcher).await;
        fs::write(root.path().join(JOURNAL), "one\ntwo\n").unwrap();
        arrives(&mut watcher).await;
    }

    #[tokio::test]
    async fn real_fs_a_missing_state_directory_is_waited_for_through_its_nearest_parent() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("state").join("nested");
        let mut watcher = watcher_at(&directory);
        assert!(!watcher.polling());
        fs::create_dir_all(&directory).unwrap();
        journal_in(&directory, "one\n");
        arrives(&mut watcher).await;
        let mut watched: Vec<_> = watcher.watches.values().cloned().collect();
        watched.sort();
        assert_eq!(
            watched,
            [directory.parent().unwrap().to_path_buf(), directory.clone()],
            "the directory is watched itself once it exists"
        );
        let file = fs::OpenOptions::new()
            .append(true)
            .open(directory.join(JOURNAL))
            .unwrap();
        std::io::Write::write_all(&mut &file, b"two\n").unwrap();
        arrives(&mut watcher).await;
    }

    #[tokio::test]
    async fn real_fs_a_directory_made_while_the_climb_passes_it_is_still_watched() {
        let root = TempDir::new().unwrap();
        let state = root.path().join("state");
        let directory = state.join("nested");
        let mut watcher = watcher_at(&directory);
        fs::create_dir(&state).unwrap();
        let mut made = false;
        watcher
            .arm_with(|fd, path, flags| {
                let added = rustix::fs::inotify::add_watch(fd, path, flags);
                if path == directory && !made {
                    made = true;
                    fs::create_dir(&directory).unwrap();
                }
                added
            })
            .unwrap();
        let mut watched: Vec<_> = watcher.watches.values().cloned().collect();
        watched.sort();
        assert_eq!(
            watched,
            [state, directory.clone()],
            "the directory made between its failed watch and its parent's is watched"
        );
        watcher.drain().unwrap();
        journal_in(&directory, "one\n");
        arrives(&mut watcher).await;
    }

    #[tokio::test]
    async fn real_fs_a_replaced_state_directory_reaches_the_loop() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("state");
        fs::create_dir(&directory).unwrap();
        journal_in(&directory, "one\n");
        let mut watcher = watcher_at(&directory);
        fs::rename(&directory, root.path().join("state-old")).unwrap();
        fs::create_dir(&directory).unwrap();
        journal_in(&directory, "two\n");
        arrives(&mut watcher).await;
        fs::write(directory.join(JOURNAL), "two\nthree\n").unwrap();
        arrives(&mut watcher).await;
    }

    #[tokio::test]
    async fn real_fs_a_state_directory_that_can_only_be_traversed_is_watched_through_the_journal() {
        if is_root() {
            return;
        }
        let root = TempDir::new().unwrap();
        journal_in(root.path(), "one\n");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o311)).unwrap();
        let _restore = Restore(root.path());
        let mut watcher = watcher_at(root.path());
        assert!(!watcher.polling());
        assert!(watcher.watches.values().any(|path| path.ends_with(JOURNAL)));
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(root.path().join(JOURNAL))
            .unwrap();
        std::io::Write::write_all(&mut file, b"two\n").unwrap();
        arrives(&mut watcher).await;
        fs::write(root.path().join("replacement"), "three\n").unwrap();
        fs::rename(root.path().join("replacement"), root.path().join(JOURNAL)).unwrap();
        arrives(&mut watcher).await;
    }

    #[tokio::test]
    async fn real_fs_a_traverse_only_directory_without_a_journal_is_polled_until_one_can_be_watched()
     {
        if is_root() {
            return;
        }
        let root = TempDir::new().unwrap();
        let directory = root.path().join("state");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o311)).unwrap();
        let _restore = Restore(&directory);
        let mut watcher = watcher_at(&directory);
        assert!(
            watcher.polling(),
            "nothing announces the journal's creation"
        );
        journal_in(&directory, "one\n");
        watcher.arm().unwrap();
        assert!(!watcher.polling());
    }

    #[tokio::test]
    async fn real_fs_an_unreadable_ancestor_is_polled_behind_only_while_the_state_directory_is_missing()
     {
        if is_root() {
            return;
        }
        let root = TempDir::new().unwrap();
        let locked = root.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o311)).unwrap();
        let _restore = Restore(&locked);
        let directory = locked.join("state");
        let mut watcher = watcher_at(&directory);
        assert!(
            watcher.polling(),
            "nothing can announce the state directory's creation"
        );
        fs::create_dir(&directory).unwrap();
        journal_in(&directory, "one\n");
        watcher.arm().unwrap();
        assert!(
            !watcher.polling(),
            "the state directory's own watch sees the journal"
        );
        assert_eq!(
            watcher.watches.values().cloned().collect::<Vec<_>>(),
            [directory]
        );
    }

    #[tokio::test]
    async fn real_fs_a_state_directory_below_a_file_is_waited_for_through_the_files_parent() {
        let root = TempDir::new().unwrap();
        let file = root.path().join("file");
        fs::write(&file, "x").unwrap();
        let runtime = tokio::runtime::Handle::current();
        let _guard = runtime.enter();
        let watcher = open(&file.join("state"));
        assert!(
            watcher.is_ok(),
            "the file's parent is watched instead: ENOTDIR climbs"
        );
    }
}
