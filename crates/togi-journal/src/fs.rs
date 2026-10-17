//! The filesystem operations the journal's durability rests on: the disk, and an in-memory tree with fault injection
//! for tests and simulations.

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustix::fs::{AtFlags, CWD, Dir, FileType, OFlags, RenameFlags};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: OsString,
    pub kind: FileKind,
}

/// A file opened for writing.
pub trait FsFile: Send + fmt::Debug {
    /// Writes every byte in one write; a short write is an error and may leave a prefix behind.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Makes every written byte durable; a no-op where the filesystem promises no durability.
    fn sync(&mut self) -> io::Result<()>;
    fn set_len(&mut self, len: u64) -> io::Result<()>;
}

/// An exclusive lock; dropping it releases the lock.
#[must_use = "dropping the lock releases it; hold it for as long as the journal is in use"]
pub struct FsLock(
    #[expect(dead_code, reason = "held only for its Drop")] Box<dyn Any + Send + Sync>,
);

impl fmt::Debug for FsLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FsLock")
    }
}

/// Every open refuses to follow a symbolic link in its final component.
pub trait Fs: Send + Sync + fmt::Debug {
    fn create_dir(&self, path: &Path) -> io::Result<()>;
    /// Opens for appending, creating the file when missing.
    fn open_append(&self, path: &Path) -> io::Result<Box<dyn FsFile>>;
    /// Creates a file that must not exist yet.
    fn create_new(&self, path: &Path) -> io::Result<Box<dyn FsFile>>;
    /// Creates a file, or empties an existing one.
    fn create_truncate(&self, path: &Path) -> io::Result<Box<dyn FsFile>>;
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.open_read(path)?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Renames, failing with `AlreadyExists` instead of replacing `to`.
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    /// Removes an empty directory without following a symbolic link.
    fn remove_dir(&self, path: &Path) -> io::Result<()>;
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
    /// Sets the permission bits of the directory at `path`, opened without following a link.
    fn set_dir_mode(&self, path: &Path, mode: u32) -> io::Result<()>;
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;
    /// The kind of the entry at `path` without following a link, or `None` when nothing is there.
    fn kind(&self, path: &Path) -> io::Result<Option<FileKind>>;
    /// Takes the exclusive lock on `path`, creating the file when missing; `None` when another holder has it.
    fn try_lock(&self, path: &Path) -> io::Result<Option<FsLock>>;
}

/// A real filesystem adapter that never syncs file contents, directory entries, or permissions.
///
/// For explicitly non-durable simulations, scratch state copies and kernel tests only; production journals use
/// [`crate::StateDir::lock`] instead.
pub fn unsynced_disk_fs() -> Arc<dyn Fs> {
    Arc::new(DiskFs::unsynced())
}

/// The real filesystem. Every sync reaches the disk unless the value was made by [`DiskFs::unsynced`].
///
/// Crate-private: writers select durability through `StateDir` or the explicit opaque unsynced adapter.
///
/// [`StateDir::lock`]: crate::StateDir::lock
#[derive(Clone, Copy, Debug)]
pub(crate) struct DiskFs {
    synced: bool,
}

impl Default for DiskFs {
    fn default() -> Self {
        Self { synced: true }
    }
}

impl DiskFs {
    pub(crate) const fn unsynced() -> Self {
        Self { synced: false }
    }

    fn options() -> OpenOptions {
        let mut options = OpenOptions::new();
        options
            .mode(0o644)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits().cast_signed());
        options
    }

    /// Opens the directory at `path`, refusing a symbolic link in its final component.
    pub(crate) fn open_dir(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::NOFOLLOW | OFlags::DIRECTORY).bits().cast_signed())
            .open(path)
    }

    fn open_write(self, options: &OpenOptions, path: &Path) -> io::Result<Box<dyn FsFile>> {
        Ok(Box::new(DiskFile {
            file: options.open(path)?,
            synced: self.synced,
        }))
    }
}

#[derive(Debug)]
struct DiskFile {
    file: File,
    synced: bool,
}

impl FsFile for DiskFile {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        loop {
            match self.file.write(bytes) {
                Ok(written) if written == bytes.len() => return Ok(()),
                Ok(_) => return Err(short_write()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "production journal acknowledgements require durable file contents"
    )]
    fn sync(&mut self) -> io::Result<()> {
        if self.synced {
            self.file.sync_data()
        } else {
            Ok(())
        }
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }
}

impl Fs for DiskFs {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::DirBuilder::new().mode(0o755).create(path)
    }

    fn open_append(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.open_write(Self::options().append(true).create(true), path)
    }

    fn create_new(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.open_write(Self::options().write(true).create_new(true), path)
    }

    fn create_truncate(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.open_write(
            Self::options().write(true).create(true).truncate(true),
            path,
        )
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(Self::options().read(true).open(path)?))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        Ok(rustix::fs::renameat_with(
            CWD,
            from,
            CWD,
            to,
            RenameFlags::NOREPLACE,
        )?)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir(path)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "production journal publication requires durable directory entries"
    )]
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        if !self.synced {
            return Ok(());
        }
        Self::open_dir(path)?.sync_all()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "production state-directory permissions must survive power loss"
    )]
    fn set_dir_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        let dir = Self::open_dir(path)?;
        dir.set_permissions(std::fs::Permissions::from_mode(mode))?;
        if self.synced { dir.sync_all() } else { Ok(()) }
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let dir = Self::open_dir(path)?;
        let mut entries = Vec::new();
        for entry in Dir::read_from(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            let file_type = match entry.file_type() {
                FileType::Unknown => FileType::from_raw_mode(
                    rustix::fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)?.st_mode,
                ),
                known @ (FileType::RegularFile
                | FileType::Directory
                | FileType::Symlink
                | FileType::Fifo
                | FileType::Socket
                | FileType::CharacterDevice
                | FileType::BlockDevice) => known,
            };
            entries.push(DirEntry {
                name: OsString::from_vec(name.to_bytes().to_vec()),
                kind: rustix_kind(file_type),
            });
        }
        Ok(entries)
    }

    fn kind(&self, path: &Path) -> io::Result<Option<FileKind>> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => Ok(Some(disk_kind(metadata.file_type()))),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn try_lock(&self, path: &Path) -> io::Result<Option<FsLock>> {
        let file = Self::options()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(FsLock(Box::new(file)))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

fn rustix_kind(kind: FileType) -> FileKind {
    match kind {
        FileType::RegularFile => FileKind::File,
        FileType::Directory => FileKind::Dir,
        FileType::Symlink => FileKind::Symlink,
        FileType::Fifo
        | FileType::Socket
        | FileType::CharacterDevice
        | FileType::BlockDevice
        | FileType::Unknown => FileKind::Other,
    }
}

fn disk_kind(kind: std::fs::FileType) -> FileKind {
    if kind.is_symlink() {
        FileKind::Symlink
    } else if kind.is_dir() {
        FileKind::Dir
    } else if kind.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    }
}

pub(crate) fn short_write() -> io::Error {
    io::Error::new(io::ErrorKind::WriteZero, "short write")
}

/// An operation [`MemFs`] counts and can fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    CreateDir,
    /// Opening a file for writing.
    Open,
    Write,
    Sync,
    SetLen,
    Rename,
    Remove,
    SyncDir,
    SetMode,
    /// Opening or listing for reading.
    Read,
}

impl Op {
    const fn mutates(self) -> bool {
        !matches!(self, Self::Read)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Before,
    After,
    Short,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Op(Op),
    AnyMutation,
}

/// One injected failure: the `nth` matching operation after injection fails, counting from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    target: Target,
    nth: u64,
    effect: Effect,
    name: Option<OsString>,
}

impl Fault {
    /// Fails the operation without performing it.
    pub fn before(op: Op, nth: u64) -> Self {
        Self {
            target: Target::Op(op),
            nth,
            effect: Effect::Before,
            name: None,
        }
    }

    /// Performs the operation, then reports it failed.
    pub fn after(op: Op, nth: u64) -> Self {
        Self {
            target: Target::Op(op),
            nth,
            effect: Effect::After,
            name: None,
        }
    }

    /// Writes the first half of the bytes, then fails.
    pub fn short_write(nth: u64) -> Self {
        Self {
            target: Target::Op(Op::Write),
            nth,
            effect: Effect::Short,
            name: None,
        }
    }

    /// Fails the `nth` mutating operation of any kind without performing it.
    pub fn any_before(nth: u64) -> Self {
        Self {
            target: Target::AnyMutation,
            nth,
            effect: Effect::Before,
            name: None,
        }
    }

    /// Performs the `nth` mutating operation of any kind, then reports it failed.
    pub fn any_after(nth: u64) -> Self {
        Self {
            target: Target::AnyMutation,
            nth,
            effect: Effect::After,
            name: None,
        }
    }

    /// Counts only operations touching a path with this file name.
    #[must_use]
    pub fn named(mut self, name: impl Into<OsString>) -> Self {
        self.name = Some(name.into());
        self
    }

    fn matches(&self, op: Op, paths: &[&Path]) -> bool {
        let op_matches = match self.target {
            Target::Op(target) => target == op,
            Target::AnyMutation => op.mutates(),
        };
        op_matches
            && self
                .name
                .as_deref()
                .is_none_or(|name| paths.iter().any(|path| path.file_name() == Some(name)))
    }
}

#[derive(Clone, Debug)]
enum Node {
    Dir,
    File(u64),
    Symlink(PathBuf),
}

#[derive(Debug, Default)]
struct History {
    log: Vec<(Op, PathBuf)>,
    reads: Vec<PathBuf>,
}

#[derive(Debug, Default)]
struct Tree {
    nodes: BTreeMap<PathBuf, Node>,
    data: HashMap<u64, Vec<u8>>,
    handles: HashMap<u64, usize>,
    next_inode: u64,
    locks: HashSet<PathBuf>,
    faults: Vec<(Fault, u64)>,
    mutations: u64,
    history: Option<History>,
}

impl Tree {
    fn check(&mut self, op: Op, paths: &[&Path]) -> Option<Effect> {
        if op.mutates() {
            self.mutations += 1;
        }
        if let Some(history) = &mut self.history {
            if op.mutates() {
                history.log.push((
                    op,
                    paths
                        .first()
                        .map(|path| path.to_path_buf())
                        .unwrap_or_default(),
                ));
            } else if let Some(path) = paths.first() {
                history.reads.push(path.to_path_buf());
            }
        }
        let mut fired = None;
        self.faults.retain_mut(|(fault, seen)| {
            if !fault.matches(op, paths) {
                return true;
            }
            *seen += 1;
            if *seen >= fault.nth.max(1) {
                fired.get_or_insert(fault.effect);
                false
            } else {
                true
            }
        });
        fired
    }

    fn history(&self) -> &History {
        self.history
            .as_ref()
            .unwrap_or_else(|| panic!("MemFs histories need record_history before the operations"))
    }

    fn node(&self, path: &Path) -> Option<&Node> {
        if path == Path::new("/") {
            Some(&Node::Dir)
        } else {
            self.nodes.get(path)
        }
    }

    fn parent_dir(&self, path: &Path) -> io::Result<()> {
        match path.parent().map(|parent| self.node(parent)) {
            Some(Some(Node::Dir)) => Ok(()),
            Some(Some(_)) => Err(io::Error::from(io::ErrorKind::NotADirectory)),
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    fn file(&self, path: &Path) -> io::Result<u64> {
        match self.node(path) {
            Some(Node::File(inode)) => Ok(*inode),
            Some(Node::Dir) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
            Some(Node::Symlink(_)) => Err(io::Error::from(rustix::io::Errno::LOOP)),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    fn reclaim(&mut self, inode: u64) {
        let open = self.handles.get(&inode).is_some_and(|count| *count > 0);
        let linked = self
            .nodes
            .values()
            .any(|node| matches!(node, Node::File(linked) if *linked == inode));
        if !open && !linked {
            self.data.remove(&inode);
            self.handles.remove(&inode);
        }
    }

    fn new_file(&mut self, path: &Path) -> io::Result<u64> {
        self.parent_dir(path)?;
        let inode = self.next_inode;
        self.next_inode += 1;
        self.data.insert(inode, Vec::new());
        self.nodes.insert(path.to_path_buf(), Node::File(inode));
        Ok(inode)
    }

    fn open(&mut self, path: &Path, mode: OpenMode) -> io::Result<u64> {
        match (self.node(path).is_some(), mode) {
            (true, OpenMode::CreateNew) => Err(io::Error::from(io::ErrorKind::AlreadyExists)),
            (true, OpenMode::Truncate) => {
                let inode = self.file(path)?;
                self.data.insert(inode, Vec::new());
                Ok(inode)
            }
            (true, OpenMode::Append) => self.file(path),
            (false, _) => self.new_file(path),
        }
    }

    fn rename(&mut self, from: &Path, to: &Path, replace: bool) -> io::Result<()> {
        let Some(node) = self.node(from).cloned() else {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        };
        self.parent_dir(to)?;
        match self.node(to) {
            Some(_) if !replace => return Err(io::Error::from(io::ErrorKind::AlreadyExists)),
            Some(Node::Dir) if !matches!(node, Node::Dir) => {
                return Err(io::Error::from(io::ErrorKind::IsADirectory));
            }
            Some(Node::Dir) if self.children(to).next().is_some() => {
                return Err(io::Error::from(io::ErrorKind::DirectoryNotEmpty));
            }
            Some(Node::File(_) | Node::Symlink(_)) if matches!(node, Node::Dir) => {
                return Err(io::Error::from(io::ErrorKind::NotADirectory));
            }
            _ => {}
        }
        let replaced = match self.node(to) {
            Some(Node::File(inode)) => Some(*inode),
            _ => None,
        };
        let moved: Vec<(PathBuf, Node)> = self
            .nodes
            .range(from.to_path_buf()..)
            .take_while(|(path, _)| path.starts_with(from))
            .map(|(path, node)| (path.clone(), node.clone()))
            .collect();
        for (path, node) in moved {
            self.nodes.remove(&path);
            let relative = path.strip_prefix(from).unwrap_or(Path::new(""));
            let destination = if relative.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(relative)
            };
            self.nodes.insert(destination, node);
        }
        if let Some(inode) = replaced {
            self.reclaim(inode);
        }
        Ok(())
    }

    fn children<'a>(&'a self, dir: &'a Path) -> impl Iterator<Item = (&'a PathBuf, &'a Node)> + 'a {
        self.nodes
            .range(dir.to_path_buf()..)
            .skip_while(move |(path, _)| path.as_path() == dir)
            .filter(move |(path, _)| path.parent() == Some(dir))
    }
}

#[derive(Clone, Copy, Debug)]
enum OpenMode {
    Append,
    CreateNew,
    Truncate,
}

/// A shareable in-memory filesystem: clones see one tree, so a simulation keeps it across simulated reboots.
#[derive(Clone, Debug, Default)]
pub struct MemFs {
    tree: Arc<Mutex<Tree>>,
}

#[derive(Debug, PartialEq, Eq)]
enum DumpEntry<'a> {
    Dir,
    File(&'a [u8]),
    Symlink(&'a Path),
}

impl MemFs {
    pub fn new() -> Self {
        Self::default()
    }

    fn tree(&self) -> MutexGuard<'_, Tree> {
        self.tree.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn inject(&self, fault: Fault) {
        self.tree().faults.push((fault, 0));
    }

    pub fn clear_faults(&self) {
        self.tree().faults.clear();
    }

    /// Fails the rename that would publish `state.json`.
    pub fn fail_state_publication(&self) {
        self.inject(Fault::before(Op::Rename, 1).named("state.json"));
    }

    /// The number of mutating operations performed so far, failed ones included, recorded or not.
    pub fn mutations(&self) -> u64 {
        self.tree().mutations
    }

    /// Starts recording the operation history [`Self::log`] and [`Self::reads`] return, for this handle and every
    /// clone. Operations before the call are not recorded; ordinary simulations never call it and keep no history.
    pub fn record_history(&self) {
        self.tree().history.get_or_insert_default();
    }

    /// Every mutating operation attempted since [`Self::record_history`], in order, with the path it named first.
    ///
    /// # Panics
    ///
    /// If histories are not being recorded, so a test cannot pass on a history it forgot to request.
    pub fn log(&self) -> Vec<(Op, PathBuf)> {
        self.tree().history().log.clone()
    }

    /// Every path opened or listed for reading since [`Self::record_history`], in order, which the mutation log
    /// leaves out.
    ///
    /// # Panics
    ///
    /// If histories are not being recorded.
    pub fn reads(&self) -> Vec<PathBuf> {
        self.tree().history().reads.clone()
    }

    /// The number of file bodies the filesystem still holds.
    pub fn stored_files(&self) -> usize {
        self.tree().data.len()
    }

    /// The bytes of the regular file at `path`.
    pub fn contents(&self, path: impl AsRef<Path>) -> Option<Vec<u8>> {
        let tree = self.tree();
        let inode = tree.file(path.as_ref()).ok()?;
        tree.data.get(&inode).cloned()
    }

    /// Writes a regular file, creating missing parent directories, outside fault injection.
    pub fn put(&self, path: impl AsRef<Path>, bytes: impl Into<Vec<u8>>) {
        let path = path.as_ref();
        let mut tree = self.tree();
        for ancestor in path.ancestors().skip(1) {
            if ancestor != Path::new("/") && !ancestor.as_os_str().is_empty() {
                tree.nodes
                    .entry(ancestor.to_path_buf())
                    .or_insert(Node::Dir);
            }
        }
        let inode = if let Some(Node::File(inode)) = tree.node(path) {
            *inode
        } else {
            let inode = tree.next_inode;
            tree.next_inode += 1;
            tree.nodes.insert(path.to_path_buf(), Node::File(inode));
            inode
        };
        tree.data.insert(inode, bytes.into());
    }

    pub fn symlink(&self, target: impl Into<PathBuf>, link: impl AsRef<Path>) {
        self.tree()
            .nodes
            .insert(link.as_ref().to_path_buf(), Node::Symlink(target.into()));
    }

    /// Every path in the tree with its kind, in path order.
    pub fn entries(&self) -> Vec<(PathBuf, FileKind)> {
        self.tree()
            .nodes
            .iter()
            .map(|(path, node)| (path.clone(), mem_kind(node)))
            .collect()
    }

    /// Writes the tree's files under `destination`, mirroring their paths.
    pub fn dump_to(&self, destination: &Path) -> io::Result<()> {
        self.dump_with(destination, |target, entry| match entry {
            DumpEntry::Dir => std::fs::create_dir_all(target),
            DumpEntry::File(bytes) => std::fs::write(target, bytes),
            DumpEntry::Symlink(link) => std::os::unix::fs::symlink(link, target),
        })
    }

    fn dump_with(
        &self,
        destination: &Path,
        mut publish: impl FnMut(&Path, DumpEntry<'_>) -> io::Result<()>,
    ) -> io::Result<()> {
        let (nodes, data) = {
            let tree = self.tree();
            (tree.nodes.clone(), tree.data.clone())
        };
        publish(destination, DumpEntry::Dir)?;
        for (path, node) in nodes {
            let target = destination.join(path.strip_prefix("/").unwrap_or(&path));
            let entry = match &node {
                Node::Dir => DumpEntry::Dir,
                Node::File(inode) => {
                    DumpEntry::File(data.get(inode).map(Vec::as_slice).unwrap_or_default())
                }
                Node::Symlink(link) => DumpEntry::Symlink(link),
            };
            publish(&target, entry)?;
        }
        Ok(())
    }

    fn mutate<T>(
        &self,
        op: Op,
        paths: &[&Path],
        apply: impl FnOnce(&mut Tree) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut tree = self.tree();
        match tree.check(op, paths) {
            Some(Effect::Before | Effect::Short) => Err(injected(op)),
            Some(Effect::After) => apply(&mut tree).and(Err(injected(op))),
            None => apply(&mut tree),
        }
    }

    fn open_file(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn FsFile>> {
        let mut tree = self.tree();
        let effect = tree.check(Op::Open, &[path]);
        if matches!(effect, Some(Effect::Before | Effect::Short)) {
            return Err(injected(Op::Open));
        }
        let inode = tree.open(path, mode)?;
        if effect.is_some() {
            return Err(injected(Op::Open));
        }
        *tree.handles.entry(inode).or_default() += 1;
        Ok(Box::new(MemFile {
            fs: self.clone(),
            path: path.to_path_buf(),
            inode,
        }))
    }
}

fn mem_kind(node: &Node) -> FileKind {
    match node {
        Node::Dir => FileKind::Dir,
        Node::File(_) => FileKind::File,
        Node::Symlink(_) => FileKind::Symlink,
    }
}

fn injected(op: Op) -> io::Error {
    io::Error::other(format!("injected {op:?} fault"))
}

#[derive(Debug)]
struct MemFile {
    fs: MemFs,
    path: PathBuf,
    inode: u64,
}

impl Drop for MemFile {
    fn drop(&mut self) {
        let mut tree = self.fs.tree();
        if let Some(count) = tree.handles.get_mut(&self.inode) {
            *count = count.saturating_sub(1);
        }
        tree.reclaim(self.inode);
    }
}

impl FsFile for MemFile {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut tree = self.fs.tree();
        let effect = tree.check(Op::Write, &[&self.path]);
        let written = match effect {
            Some(Effect::Before) => return Err(injected(Op::Write)),
            Some(Effect::Short) => &bytes[..bytes.len() / 2],
            Some(Effect::After) | None => bytes,
        };
        tree.data
            .entry(self.inode)
            .or_default()
            .extend_from_slice(written);
        match effect {
            Some(Effect::Short) => Err(short_write()),
            Some(Effect::After) => Err(injected(Op::Write)),
            _ => Ok(()),
        }
    }

    fn sync(&mut self) -> io::Result<()> {
        self.fs.mutate(Op::Sync, &[&self.path], |_| Ok(()))
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        let inode = self.inode;
        self.fs.mutate(Op::SetLen, &[&self.path], |tree| {
            let data = tree.data.entry(inode).or_default();
            data.resize(usize::try_from(len).map_err(io::Error::other)?, 0);
            Ok(())
        })
    }
}

struct MemLock {
    fs: MemFs,
    path: PathBuf,
}

impl Drop for MemLock {
    fn drop(&mut self) {
        self.fs.tree().locks.remove(&self.path);
    }
}

impl Fs for MemFs {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.mutate(Op::CreateDir, &[path], |tree| {
            if tree.node(path).is_some() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            tree.parent_dir(path)?;
            tree.nodes.insert(path.to_path_buf(), Node::Dir);
            Ok(())
        })
    }

    fn open_append(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.open_file(path, OpenMode::Append)
    }

    fn create_new(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.open_file(path, OpenMode::CreateNew)
    }

    fn create_truncate(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.open_file(path, OpenMode::Truncate)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        let bytes = self.mutate(Op::Read, &[path], |tree| {
            let inode = tree.file(path)?;
            Ok(tree.data.get(&inode).cloned().unwrap_or_default())
        })?;
        Ok(Box::new(io::Cursor::new(bytes)))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.mutate(Op::Rename, &[from, to], |tree| tree.rename(from, to, true))
    }

    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.mutate(Op::Rename, &[from, to], |tree| tree.rename(from, to, false))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.mutate(Op::Remove, &[path], |tree| match tree.node(path) {
            Some(Node::Dir) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
            Some(_) => {
                if let Some(Node::File(inode)) = tree.nodes.remove(path) {
                    tree.reclaim(inode);
                }
                Ok(())
            }
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        })
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.mutate(Op::Remove, &[path], |tree| match tree.node(path) {
            Some(Node::Dir) => {
                if path == Path::new("/") {
                    return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                }
                if tree.children(path).next().is_some() {
                    return Err(io::Error::from(io::ErrorKind::DirectoryNotEmpty));
                }
                tree.nodes.remove(path);
                Ok(())
            }
            Some(_) => Err(io::Error::from(io::ErrorKind::NotADirectory)),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        })
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.mutate(Op::SyncDir, &[path], |tree| match tree.node(path) {
            Some(Node::Dir) => Ok(()),
            Some(_) => Err(io::Error::from(io::ErrorKind::NotADirectory)),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        })
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.mutate(Op::Read, &[path], |tree| match tree.node(path) {
            Some(Node::Dir) => Ok(tree
                .children(path)
                .filter_map(|(child, node)| {
                    Some(DirEntry {
                        name: child.file_name()?.to_os_string(),
                        kind: mem_kind(node),
                    })
                })
                .collect()),
            Some(_) => Err(io::Error::from(io::ErrorKind::NotADirectory)),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        })
    }

    fn kind(&self, path: &Path) -> io::Result<Option<FileKind>> {
        Ok(self.tree().node(path).map(mem_kind))
    }

    fn try_lock(&self, path: &Path) -> io::Result<Option<FsLock>> {
        let mut tree = self.tree();
        let effect = tree.check(Op::Open, &[path]);
        if matches!(effect, Some(Effect::Before | Effect::Short)) {
            return Err(injected(Op::Open));
        }
        if tree.node(path).is_none() {
            tree.new_file(path)?;
        } else {
            tree.file(path)?;
        }
        if !tree.locks.insert(path.to_path_buf()) {
            return Ok(None);
        }
        if effect.is_some() {
            tree.locks.remove(path);
            return Err(injected(Op::Open));
        }
        Ok(Some(FsLock(Box::new(MemLock {
            fs: self.clone(),
            path: path.to_path_buf(),
        }))))
    }

    fn set_dir_mode(&self, path: &Path, _mode: u32) -> io::Result<()> {
        self.mutate(Op::SetMode, &[path], |tree| match tree.node(path) {
            Some(Node::Dir) => Ok(()),
            Some(_) => Err(io::Error::from(io::ErrorKind::NotADirectory)),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(fs: &MemFs, path: &str) {
        fs.create_dir(Path::new(path)).unwrap();
    }

    #[test]
    fn appends_land_in_order_and_survive_clones() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        let mut file = fs.open_append(Path::new("/s/a")).unwrap();
        file.write_all(b"one\n").unwrap();
        file.sync().unwrap();
        let shared = fs.clone();
        let mut again = shared.open_append(Path::new("/s/a")).unwrap();
        again.write_all(b"two\n").unwrap();
        assert_eq!(fs.contents("/s/a").unwrap(), b"one\ntwo\n");
    }

    #[test]
    fn short_write_leaves_a_prefix_and_fails() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        let mut file = fs.open_append(Path::new("/s/a")).unwrap();
        fs.inject(Fault::short_write(1));
        let error = file.write_all(b"abcd").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        assert_eq!(fs.contents("/s/a").unwrap(), b"ab");
    }

    #[test]
    fn faults_fire_on_the_nth_matching_operation_only() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        let mut file = fs.open_append(Path::new("/s/a")).unwrap();
        fs.inject(Fault::before(Op::Sync, 2));
        file.sync().unwrap();
        assert!(file.sync().is_err());
        file.sync().unwrap();
    }

    #[test]
    fn after_effect_faults_apply_then_fail() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        fs.put("/s/a", "x");
        fs.inject(Fault::after(Op::Rename, 1));
        assert!(fs.rename(Path::new("/s/a"), Path::new("/s/b")).is_err());
        assert_eq!(fs.contents("/s/b").unwrap(), b"x");
        assert!(fs.contents("/s/a").is_none());
    }

    #[test]
    fn named_faults_skip_other_files() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        fs.put("/s/a", "x");
        fs.put("/s/.state.json.tmp", "y");
        fs.fail_state_publication();
        fs.rename(Path::new("/s/a"), Path::new("/s/b")).unwrap();
        assert!(
            fs.rename(Path::new("/s/.state.json.tmp"), Path::new("/s/state.json"))
                .is_err()
        );
        assert!(fs.contents("/s/state.json").is_none());
    }

    #[test]
    fn any_mutation_faults_count_every_kind() {
        let fs = MemFs::new();
        fs.inject(Fault::any_before(2));
        dir(&fs, "/s");
        assert!(fs.create_dir(Path::new("/s/t")).is_err());
        assert_eq!(fs.mutations(), 2);
    }

    fn exercise(fs: &MemFs) -> (Vec<bool>, u64) {
        dir(fs, "/s");
        fs.inject(Fault::before(Op::Sync, 2));
        fs.inject(Fault::any_after(3));
        let mut file = fs.open_append(Path::new("/s/a")).unwrap();
        let outcomes = vec![
            file.write_all(b"x").is_ok(),
            file.sync().is_ok(),
            file.sync().is_ok(),
            fs.read(Path::new("/s/a")).is_ok(),
            fs.rename(Path::new("/s/a"), Path::new("/s/b")).is_ok(),
        ];
        (outcomes, fs.mutations())
    }

    #[test]
    fn recording_histories_changes_no_fault_or_mutation_count() {
        let recorded = MemFs::new();
        recorded.record_history();
        let want = (vec![true, false, false, true, true], 6);
        assert_eq!(exercise(&MemFs::new()), want);
        assert_eq!(exercise(&recorded), want);
    }

    #[test]
    fn histories_are_not_recorded_by_default() {
        let fs = MemFs::new();
        exercise(&fs);
        assert!(fs.tree().history.is_none());
    }

    #[test]
    #[should_panic(expected = "record_history")]
    fn an_unrequested_history_cannot_be_read() {
        MemFs::new().log();
    }

    #[test]
    fn clones_record_in_order_from_the_opt_in() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        fs.read_dir(Path::new("/")).unwrap();
        let shared = fs.clone();
        shared.record_history();
        let mut file = fs.open_append(Path::new("/s/a")).unwrap();
        file.sync().unwrap();
        fs.read_dir(Path::new("/s")).unwrap();
        fs.rename(Path::new("/s/a"), Path::new("/s/b")).unwrap();
        fs.read(Path::new("/s/b")).unwrap();
        assert_eq!(
            shared.log(),
            [
                (Op::Open, PathBuf::from("/s/a")),
                (Op::Sync, PathBuf::from("/s/a")),
                (Op::Rename, PathBuf::from("/s/a")),
            ]
        );
        assert_eq!(fs.reads(), [PathBuf::from("/s"), PathBuf::from("/s/b")]);
    }

    #[test]
    fn rename_noreplace_refuses_an_existing_target() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        fs.put("/s/a", "x");
        fs.put("/s/b", "y");
        let error = fs
            .rename_noreplace(Path::new("/s/a"), Path::new("/s/b"))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs.contents("/s/b").unwrap(), b"y");
    }

    #[test]
    fn renaming_a_directory_moves_its_children() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        dir(&fs, "/s/t");
        fs.put("/s/t/x", "1");
        fs.rename_noreplace(Path::new("/s/t"), Path::new("/s/u"))
            .unwrap();
        assert_eq!(fs.contents("/s/u/x").unwrap(), b"1");
        assert_eq!(fs.kind(Path::new("/s/t")).unwrap(), None);
    }

    #[test]
    fn handles_follow_the_inode_across_renames() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        let mut file = fs.open_append(Path::new("/s/a")).unwrap();
        fs.rename(Path::new("/s/a"), Path::new("/s/b")).unwrap();
        file.write_all(b"z").unwrap();
        assert_eq!(fs.contents("/s/b").unwrap(), b"z");
    }

    #[test]
    fn opens_refuse_symlinks() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        fs.put("/s/real", "x");
        fs.symlink("/s/real", "/s/link");
        assert!(fs.open_read(Path::new("/s/link")).is_err());
        assert!(fs.open_append(Path::new("/s/link")).is_err());
        assert_eq!(
            fs.kind(Path::new("/s/link")).unwrap(),
            Some(FileKind::Symlink)
        );
    }

    #[test]
    fn read_dir_lists_direct_children_with_kinds() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        dir(&fs, "/s/d");
        fs.put("/s/d/inner", "");
        fs.put("/s/f", "");
        let mut entries = fs.read_dir(Path::new("/s")).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(
            entries,
            [
                DirEntry {
                    name: "d".into(),
                    kind: FileKind::Dir
                },
                DirEntry {
                    name: "f".into(),
                    kind: FileKind::File
                }
            ]
        );
    }

    #[test]
    fn locks_are_exclusive_until_dropped() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        let held = fs.try_lock(Path::new("/s/lock")).unwrap();
        assert!(held.is_some());
        assert!(fs.clone().try_lock(Path::new("/s/lock")).unwrap().is_none());
        drop(held);
        assert!(fs.try_lock(Path::new("/s/lock")).unwrap().is_some());
    }

    #[test]
    fn create_dir_needs_a_parent() {
        let fs = MemFs::new();
        assert_eq!(
            fs.create_dir(Path::new("/a/b")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn dump_to_mirrors_the_tree() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        fs.put("/s/a/b", "bytes");
        fs.symlink("/s/a/b", "/s/link");
        let out = MemFs::new();
        fs.dump_with(Path::new("/out"), |target, entry| {
            match entry {
                DumpEntry::Dir => out.create_dir(target)?,
                DumpEntry::File(bytes) => out.put(target, bytes),
                DumpEntry::Symlink(link) => out.symlink(link, target),
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(out.contents("/out/s/a/b").unwrap(), b"bytes");
        assert_eq!(
            out.kind(Path::new("/out/s/link")).unwrap(),
            Some(FileKind::Symlink)
        );
        assert!(out.open_read(Path::new("/out/s/link")).is_err());
    }

    #[test]
    fn removing_a_directory_requires_an_empty_directory_not_a_link() {
        let fs = MemFs::new();
        fs.put("/s/inner/file", "bytes");
        fs.symlink("/s/inner", "/s/link");
        assert_eq!(
            fs.remove_dir(Path::new("/s/inner")).unwrap_err().kind(),
            io::ErrorKind::DirectoryNotEmpty
        );
        assert_eq!(
            fs.remove_dir(Path::new("/s/link")).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        fs.remove_file(Path::new("/s/inner/file")).unwrap();
        fs.inject(Fault::before(Op::Remove, 1));
        assert!(fs.remove_dir(Path::new("/s/inner")).is_err());
        assert_eq!(fs.kind(Path::new("/s/inner")).unwrap(), Some(FileKind::Dir));
        fs.remove_dir(Path::new("/s/inner")).unwrap();
        assert_eq!(fs.kind(Path::new("/s/inner")).unwrap(), None);
        assert_eq!(
            fs.remove_dir(Path::new("/s/inner")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn real_fs_disk_locks_are_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let held = DiskFs::unsynced().try_lock(&path).unwrap();
        assert!(held.is_some());
        assert!(DiskFs::unsynced().try_lock(&path).unwrap().is_none());
        drop(held);
        assert!(DiskFs::unsynced().try_lock(&path).unwrap().is_some());
    }

    #[test]
    fn real_fs_disk_rename_noreplace_refuses_an_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "x").unwrap();
        std::fs::write(dir.path().join("b"), "y").unwrap();
        let error = DiskFs::unsynced()
            .rename_noreplace(&dir.path().join("a"), &dir.path().join("b"))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn real_fs_disk_opens_refuse_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real"), "x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        assert!(
            DiskFs::unsynced()
                .open_read(&dir.path().join("link"))
                .is_err()
        );
        assert_eq!(
            DiskFs::unsynced().kind(&dir.path().join("link")).unwrap(),
            Some(FileKind::Symlink)
        );
    }

    #[test]
    fn replaced_and_removed_files_release_their_bytes() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        for round in 0..100 {
            let mut file = fs.create_truncate(Path::new("/s/.tmp")).unwrap();
            file.write_all(b"document").unwrap();
            drop(file);
            fs.rename(Path::new("/s/.tmp"), Path::new("/s/state.json"))
                .unwrap();
            assert!(
                fs.stored_files() <= 2,
                "round {round}: {} stored",
                fs.stored_files()
            );
        }
        fs.remove_file(Path::new("/s/state.json")).unwrap();
        assert_eq!(fs.stored_files(), 0);
    }

    #[test]
    fn an_open_handle_keeps_a_removed_file_until_it_drops() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        let mut file = fs.open_append(Path::new("/s/a")).unwrap();
        fs.remove_file(Path::new("/s/a")).unwrap();
        file.write_all(b"x").unwrap();
        assert_eq!(fs.stored_files(), 1);
        drop(file);
        assert_eq!(fs.stored_files(), 0);
    }

    #[test]
    fn a_lock_that_fails_after_taking_effect_is_not_held() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        fs.inject(Fault::after(Op::Open, 1));
        assert!(fs.try_lock(Path::new("/s/lock")).is_err());
        assert!(fs.try_lock(Path::new("/s/lock")).unwrap().is_some());
    }

    #[test]
    fn every_queued_fault_counts_each_matching_operation() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        let mut file = fs.open_append(Path::new("/s/a")).unwrap();
        fs.inject(Fault::before(Op::Sync, 1));
        fs.inject(Fault::before(Op::Sync, 2));
        assert!(file.sync().is_err());
        assert!(file.sync().is_err());
        file.sync().unwrap();
    }

    #[test]
    fn an_open_that_fails_after_taking_effect_holds_no_handle() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        fs.inject(Fault::after(Op::Open, 1));
        assert!(fs.open_append(Path::new("/s/a")).is_err());
        fs.remove_file(Path::new("/s/a")).unwrap();
        assert_eq!(fs.stored_files(), 0);
    }

    #[test]
    fn create_new_refuses_an_existing_file() {
        let fs = MemFs::new();
        dir(&fs, "/s");
        fs.put("/s/a", "x");
        let error = fs.create_new(Path::new("/s/a")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs.contents("/s/a").unwrap(), b"x");
    }

    #[test]
    fn real_fs_disk_read_dir_refuses_a_symlinked_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::fs::write(dir.path().join("real/inner"), "x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        assert!(
            DiskFs::unsynced()
                .read_dir(&dir.path().join("link"))
                .is_err()
        );
        let entries = DiskFs::unsynced()
            .read_dir(&dir.path().join("real"))
            .unwrap();
        assert_eq!(
            entries,
            [DirEntry {
                name: "inner".into(),
                kind: FileKind::File
            }]
        );
    }

    #[test]
    fn disallowed_io_negative_controls_are_never_executed() {
        #[expect(
            clippy::disallowed_methods,
            reason = "pins the disk simulation tripwire without executing it"
        )]
        fn unsynced(path: &Path) -> Result<crate::StateDir, crate::LockError> {
            crate::StateDir::lock_unsynced(path)
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "pins the opaque unsynced adapter tripwire without opening anything"
        )]
        fn disk() -> Arc<dyn Fs> {
            unsynced_disk_fs()
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "pins the generic filesystem constructor tripwire without executing it"
        )]
        fn with_fs(fs: Arc<dyn Fs>, path: &Path) -> Result<crate::StateDir, crate::LockError> {
            crate::StateDir::lock_with_fs(fs, path)
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "pins the simulation dump tripwire without executing it"
        )]
        fn dump(fs: &MemFs, path: &Path) -> io::Result<()> {
            fs.dump_to(path)
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "pins the sync_all tripwire without creating a descriptor or syncing"
        )]
        fn sync_all(file: &File) -> io::Result<()> {
            file.sync_all()
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "pins the sync_data tripwire without creating a descriptor or syncing"
        )]
        fn sync_data(file: &File) -> io::Result<()> {
            file.sync_data()
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "pins the rustix fsync tripwire without creating a descriptor or syncing"
        )]
        fn fsync(file: &File) -> rustix::io::Result<()> {
            rustix::fs::fsync(file)
        }
        #[cfg(target_os = "linux")]
        #[expect(
            clippy::disallowed_methods,
            reason = "pins the rustix fdatasync tripwire without creating a descriptor or syncing"
        )]
        fn fdatasync(file: &File) -> rustix::io::Result<()> {
            rustix::fs::fdatasync(file)
        }
        // Referencing the functions keeps them type-checked and linted without calling any of them.
        let _ = (unsynced, disk, with_fs, dump, sync_all, sync_data, fsync);
        #[cfg(target_os = "linux")]
        let _ = fdatasync;
    }
}
