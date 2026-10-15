use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rustix::fs::{AtFlags, Dir, Gid, Mode, OFlags, Uid, fchmod, fchown, mkdirat, openat, unlinkat};
use rustix::io::Errno;
use togi_backend::{Backend, Plan};
use togi_journal::fs::{FileKind, Fs, FsFile};
use togi_machine::Workload;

use crate::error::PrepareError;
use crate::id::TrialId;
use crate::identity::BackendIdentity;
use crate::plan::InstancePlan;
use crate::samples::{Durability, Open, create_file, create_file_unsynced};

const MAX_DEPTH: usize = 128;

type ChownFn = dyn Fn(&str, Option<BorrowedFd<'_>>, Uid, Gid) -> Result<(), Errno> + Send + Sync;

/// How a created file or directory is handed to the backend account; the target names it for
/// whoever observes the call. Production changes the owner of the descriptor it was given.
#[derive(Clone)]
pub(crate) struct Chown(Arc<ChownFn>);

impl Chown {
    pub(crate) fn new(
        change: impl Fn(&str, Option<BorrowedFd<'_>>, Uid, Gid) -> Result<(), Errno>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self(Arc::new(change))
    }

    fn apply(
        &self,
        target: &str,
        fd: Option<BorrowedFd<'_>>,
        uid: Uid,
        gid: Gid,
    ) -> Result<(), Errno> {
        (self.0)(target, fd, uid, gid)
    }
}

impl Default for Chown {
    fn default() -> Self {
        Self::new(|_, fd, uid, gid| match fd {
            Some(fd) => fchown(fd, Some(uid), Some(gid)),
            None => Ok(()),
        })
    }
}

impl std::fmt::Debug for Chown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Chown")
    }
}

/// The trial's root-owned directory, held by descriptor so nothing below is reached by path.
/// Disk storage always retains both directory descriptors; memory storage needs neither.
#[derive(Debug)]
pub(crate) struct Container {
    pub(crate) path: PathBuf,
    storage: ContainerStorage,
    pub(crate) chown: Chown,
}

#[derive(Debug)]
enum ContainerStorage {
    Disk {
        dir: Arc<OwnedFd>,
        parent: Arc<OwnedFd>,
    },
    Memory(Arc<dyn Fs>),
}

/// One instance's directory, its retained logs and what the backend asked for.
pub(crate) struct InstanceFiles {
    pub(crate) path: PathBuf,
    pub(crate) dir: Option<Arc<OwnedFd>>,
    pub(crate) stdout: crate::logs::Sink,
    pub(crate) stderr: crate::logs::Sink,
    pub(crate) plan: Plan,
    /// Generated input paths relative to the trial directory.
    pub(crate) inputs: Vec<String>,
}

impl std::fmt::Debug for InstanceFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceFiles")
            .field("path", &self.path)
            .field("dir", &self.dir)
            .field("plan", &self.plan)
            .field("inputs", &self.inputs)
            .finish_non_exhaustive()
    }
}

pub(crate) struct MemorySink(pub(crate) Box<dyn FsFile>);

impl io::Write for MemorySink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn directory_error(
    operation: &'static str,
    path: &Path,
    source: impl Into<io::Error>,
) -> PrepareError {
    PrepareError::Directory {
        operation,
        path: path.to_owned(),
        source: source.into(),
    }
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(io::Error::other)
}

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn open_directory(parent: BorrowedFd<'_>, name: &str) -> Result<OwnedFd, Errno> {
    openat(parent, name, DIRECTORY, Mode::empty())
}

/// Removes `name` below `dir` without ever following a symlink.
fn remove_entry(dir: BorrowedFd<'_>, name: &CString, depth: usize) -> io::Result<()> {
    match unlinkat(dir, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => return Ok(()),
        Err(Errno::ISDIR | Errno::PERM) => {}
        Err(errno) => return Err(errno.into()),
    }
    if depth >= MAX_DEPTH {
        return Err(io::Error::other("directory nesting too deep"));
    }
    let child = openat(dir, name, DIRECTORY, Mode::empty())?;
    let mut names = Vec::new();
    for entry in Dir::read_from(&child)? {
        let entry = entry?;
        let entry_name = entry.file_name();
        if entry_name != c"." && entry_name != c".." {
            names.push(entry_name.to_owned());
        }
    }
    for entry in &names {
        remove_entry(child.as_fd(), entry, depth + 1)?;
    }
    unlinkat(dir, name, AtFlags::REMOVEDIR)?;
    Ok(())
}

/// What the walk from `/` recorded about one directory it held open.
#[derive(Clone, Debug)]
struct Link {
    path: PathBuf,
    uid: u32,
    gid: u32,
    mode: u32,
}

fn link_of(fd: &OwnedFd, path: &Path) -> Result<Link, PrepareError> {
    let stat = rustix::fs::fstat(fd).map_err(|error| directory_error("stat", path, error))?;
    Ok(Link {
        path: path.to_owned(),
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat.st_mode & 0o7777,
    })
}

/// Whether the account, classed as the directory's owner, its group or everybody else (never more
/// than one class), may search the directory.
fn searchable(link: &Link, identity: BackendIdentity) -> bool {
    let bit = if link.uid == identity.uid() {
        0o100
    } else if link.gid == identity.gid() {
        0o010
    } else {
        0o001
    };
    link.mode & bit != 0
}

/// Every directory from `/` to the trial directory must be one the account can search.
fn check_search(chain: &[Link], identity: BackendIdentity) -> Result<(), PrepareError> {
    match chain.iter().rev().find(|link| !searchable(link, identity)) {
        None => Ok(()),
        Some(link) => Err(PrepareError::Inaccessible {
            path: link.path.clone(),
            uid: identity.uid(),
            gid: identity.gid(),
        }),
    }
}

/// Where a directory of the walk sits, which decides who may own it and write to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// `/`: trusted whoever owns it, since no other namespace can replace it.
    NamespaceRoot,
    /// Above the state directory: owned by root or this process's user; others may write to it
    /// only through the sticky bit, which stops them replacing entries they do not own (and each
    /// entry below must itself pass).
    Ancestor,
    /// The state directory, parent of the trials directory: root or this user, nobody else writes.
    State,
    /// The trials directory: this user only, nobody else writes.
    Trials,
}

fn role(index: usize, last: usize) -> Role {
    if index == last {
        Role::Trials
    } else if index + 1 == last {
        Role::State
    } else if index == 0 {
        Role::NamespaceRoot
    } else {
        Role::Ancestor
    }
}

/// Why a directory may not stand in the trials path, `None` when it may.
fn refusal(mode: u32, owner: u32, caller: u32, role: Role) -> Option<&'static str> {
    let root_may_own = role != Role::Trials;
    let authorized = owner == caller || (root_may_own && owner == 0) || role == Role::NamespaceRoot;
    if !authorized {
        return Some("it belongs to another user");
    }
    let sticky_covers = matches!(role, Role::NamespaceRoot | Role::Ancestor) && mode & 0o1000 != 0;
    (mode & 0o022 != 0 && !sticky_covers).then_some("others can write to it")
}

/// Opens `name` below `dir` as a directory, never through a symlink; a missing one is created
/// (0755 whatever the umask) when `create` is set.
fn descend(dir: &OwnedFd, name: &OsStr, at: &Path, create: bool) -> Result<OwnedFd, PrepareError> {
    let path = at.join(name);
    let text = name.to_str().ok_or_else(|| {
        directory_error(
            "open",
            &path,
            io::Error::new(io::ErrorKind::InvalidInput, "the name is not UTF-8"),
        )
    })?;
    match open_directory(dir.as_fd(), text) {
        Ok(fd) => Ok(fd),
        Err(Errno::NOENT) if create => {
            let created = match mkdirat(dir, text, Mode::from_raw_mode(0o755)) {
                Ok(()) => true,
                Err(Errno::EXIST) => false,
                Err(error) => return Err(directory_error("create", &path, error)),
            };
            let fd = open_directory(dir.as_fd(), text)
                .map_err(|error| directory_error("open", &path, error))?;
            if created {
                fchmod(&fd, Mode::from_raw_mode(0o755))
                    .map_err(|error| directory_error("set the mode of", &path, error))?;
            }
            Ok(fd)
        }
        Err(error) => Err(directory_error("open", &path, error)),
    }
}

/// Opens every directory of `path` from `/` by descriptor, one `openat(O_NOFOLLOW)` per
/// component, and judges each by `refusal` before going below it. `..` is resolved lexically,
/// which is exact because no component of the walk is a symlink.
fn walk(path: &Path, create: bool, caller: u32) -> Result<(OwnedFd, Vec<Link>), PrepareError> {
    let mut names: Vec<&OsStr> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => names.push(name),
            Component::ParentDir => {
                names.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    if names.is_empty() {
        return Err(PrepareError::NotDirectory {
            path: path.to_owned(),
        });
    }
    let mut at = PathBuf::from("/");
    let mut dir = rustix::fs::open("/", DIRECTORY & !OFlags::NOFOLLOW, Mode::empty())
        .map_err(|error| directory_error("open", &at, error))?;
    let last = names.len();
    let mut chain = Vec::with_capacity(last + 1);
    for (index, name) in names.iter().copied().map(Some).chain([None]).enumerate() {
        let link = link_of(&dir, &at)?;
        if let Some(reason) = refusal(link.mode, link.uid, caller, role(index, last)) {
            return Err(PrepareError::Untrusted { path: at, reason });
        }
        chain.push(link);
        let Some(name) = name else { break };
        dir = descend(&dir, name, &at, create)?;
        at.push(name);
    }
    Ok((dir, chain))
}

/// Removes any earlier trial of this id without following symlinks, recreates it, fixes its mode
/// (0711 when scoped, 0755 otherwise) by fchmod, and checks that the backend account can search
/// down to it.
pub(crate) fn recreate(
    root: &Path,
    id: &TrialId,
    identity: Option<BackendIdentity>,
) -> Result<Container, PrepareError> {
    let TrialsRoot {
        path: root,
        dir: parent,
        mut chain,
    } = TrialsRoot::open(root, true)?;
    let mode = Mode::from_raw_mode(if identity.is_some() { 0o711 } else { 0o755 });
    fchmod(&parent, mode).map_err(|error| directory_error("set the mode of", &root, error))?;
    let path = root.join(id.as_str());
    let name =
        c_name(OsStr::new(id.as_str())).map_err(|error| directory_error("name", &path, error))?;
    remove_entry(parent.as_fd(), &name, 0)
        .map_err(|error| directory_error("remove the earlier trial", &path, error))?;
    mkdirat(&parent, &name, Mode::from_raw_mode(0o755))
        .map_err(|error| directory_error("create", &path, error))?;
    let dir = open_directory(parent.as_fd(), id.as_str())
        .map_err(|error| directory_error("open", &path, error))?;
    fchmod(&dir, mode).map_err(|error| directory_error("set the mode of", &path, error))?;
    if let Some(identity) = identity {
        chain.pop();
        chain.push(link_of(&parent, &root)?);
        chain.push(link_of(&dir, &path)?);
        check_search(&chain, identity)?;
    }
    Ok(Container {
        path,
        storage: ContainerStorage::Disk {
            dir: Arc::new(dir),
            parent: Arc::new(parent),
        },
        chown: Chown::default(),
    })
}

/// Uses the supplied in-memory filesystem; the production path retains descriptor validation.
pub(crate) fn recreate_in(
    root: &Path,
    id: &TrialId,
    identity: Option<BackendIdentity>,
    fs: Option<Arc<dyn Fs>>,
) -> Result<Container, PrepareError> {
    fn directory(fs: &dyn Fs, path: &Path) -> io::Result<()> {
        if fs.kind(path)? == Some(FileKind::Dir) {
            return Ok(());
        }
        if let Some(parent) = path.parent().filter(|parent| *parent != path) {
            directory(fs, parent)?;
        }
        fs.create_dir(path)
    }
    fn remove(fs: &dyn Fs, path: &Path) -> io::Result<()> {
        if fs.kind(path)? == Some(FileKind::Dir) {
            for entry in fs.read_dir(path)? {
                remove(fs, &path.join(entry.name))?;
            }
            fs.remove_dir(path)
        } else {
            fs.remove_file(path)
        }
    }
    let Some(fs) = fs else {
        return recreate(root, id, identity);
    };
    directory(&*fs, root).map_err(|error| directory_error("create", root, error))?;
    let path = root.join(id.as_str());
    if fs
        .kind(&path)
        .map_err(|error| directory_error("inspect", &path, error))?
        .is_some()
    {
        remove(&*fs, &path)
            .map_err(|error| directory_error("remove the earlier trial", &path, error))?;
    }
    fs.create_dir(&path)
        .map_err(|error| directory_error("create", &path, error))?;
    Ok(Container {
        path,
        storage: ContainerStorage::Memory(fs),
        chown: Chown::default(),
    })
}

/// The directory holding every trial, reached by descriptors from `/`: no component is a symlink,
/// every directory on the way belongs to root or this process's user and is writable by nobody
/// else (or only through the sticky bit, above the state directory), so no other account can
/// replace an entry on the path; the state directory and the trials directory admit no other
/// writer at all.
pub(crate) struct TrialsRoot {
    pub(crate) path: PathBuf,
    pub(crate) dir: OwnedFd,
    chain: Vec<Link>,
}

impl TrialsRoot {
    pub(crate) fn open(root: &Path, create: bool) -> Result<Self, PrepareError> {
        let base = if root.is_absolute() {
            PathBuf::from("/")
        } else {
            std::env::current_dir().map_err(|error| directory_error("resolve", root, error))?
        };
        Self::open_from(&base, root, create, rustix::process::geteuid().as_raw())
    }

    /// `root`, relative to `base` when it is not absolute, judged for the user `caller`.
    fn open_from(
        base: &Path,
        root: &Path,
        create: bool,
        caller: u32,
    ) -> Result<Self, PrepareError> {
        let (dir, chain) = walk(&base.join(root), create, caller)?;
        let path = chain
            .last()
            .map_or_else(PathBuf::new, |link| link.path.clone());
        Ok(Self { path, dir, chain })
    }

    /// One trial's directory below the root, never through a symlink.
    pub(crate) fn trial(&self, id: &TrialId) -> Result<OwnedFd, PrepareError> {
        open_directory(self.dir.as_fd(), id.as_str())
            .map_err(|error| directory_error("open", &self.path.join(id.as_str()), error))
    }

    /// The trial's conditions samples, plain or compressed, read through validated descriptors:
    /// a symlink, FIFO or other nonregular file is never opened for reading.
    pub(crate) fn samples(&self, id: &TrialId) -> Option<Box<dyn io::Read>> {
        let trial = self.trial(id).ok()?;
        let open = |name: &str| -> Result<File, Errno> {
            let fd = openat(
                &trial,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            let stat = rustix::fs::fstat(&fd)?;
            if rustix::fs::FileType::from_raw_mode(stat.st_mode)
                != rustix::fs::FileType::RegularFile
            {
                return Err(Errno::INVAL);
            }
            Ok(File::from(fd))
        };
        match open(togi_trialfiles::SAMPLES) {
            Ok(file) => Some(Box::new(file)),
            Err(Errno::NOENT) => {
                let compressed = open(&format!("{}.gz", togi_trialfiles::SAMPLES)).ok()?;
                Some(Box::new(flate2::read::MultiGzDecoder::new(compressed)))
            }
            Err(_) => None,
        }
    }
}

fn create_exclusive(dir: BorrowedFd<'_>, name: &str) -> Result<OwnedFd, Errno> {
    openat(
        dir,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o644),
    )
}

impl Container {
    /// Creates the instance directory (0755), holds both logs `O_EXCL`, creates the inputs, then
    /// hands the inputs and finally the directory to the backend account.
    pub(crate) fn instance(
        &self,
        plan: &InstancePlan,
        backend: &dyn Backend,
        workload: &Workload,
        identity: Option<BackendIdentity>,
    ) -> Result<InstanceFiles, PrepareError> {
        let trial = match &self.storage {
            ContainerStorage::Disk { dir, .. } => dir,
            ContainerStorage::Memory(fs) => {
                return self.instance_in(plan, backend, workload, identity, &**fs);
            }
        };
        let name = plan.name.dir_name();
        let path = self.path.join(&name);
        let create = |what: &str, source: Errno| PrepareError::Create {
            instance: name.clone(),
            name: what.to_owned(),
            source: source.into(),
        };
        mkdirat(&**trial, name.as_str(), Mode::from_raw_mode(0o755))
            .map_err(|errno| create(&name, errno))?;
        let dir = open_directory(trial.as_fd(), &name).map_err(|errno| create(&name, errno))?;
        fchmod(&dir, Mode::from_raw_mode(0o755)).map_err(|errno| create(&name, errno))?;
        let backend_plan = backend
            .plan(workload, &path, &plan.cpus)
            .map_err(PrepareError::Backend)?;
        if backend_plan.program.as_os_str().is_empty() {
            return Err(PrepareError::EmptyProgram);
        }
        let stdout = File::from(
            create_exclusive(dir.as_fd(), "stdout.log")
                .map_err(|errno| create("stdout.log", errno))?,
        );
        let stderr = File::from(
            create_exclusive(dir.as_fd(), "stderr.log")
                .map_err(|errno| create("stderr.log", errno))?,
        );
        let owner =
            identity.map(|identity| (Uid::from_raw(identity.uid()), Gid::from_raw(identity.gid())));
        let chown_failed = |target: &str, errno: Errno| PrepareError::Chown {
            instance: name.clone(),
            name: target.to_owned(),
            source: errno.into(),
        };
        let mut inputs = Vec::with_capacity(backend_plan.inputs.len());
        for input in &backend_plan.inputs {
            let file_name = input.name.as_str();
            let fd = create_exclusive(dir.as_fd(), file_name)
                .map_err(|errno| create(file_name, errno))?;
            let mut file = File::from(fd);
            file.write_all(&input.contents)
                .map_err(|source| PrepareError::Create {
                    instance: name.clone(),
                    name: file_name.to_owned(),
                    source,
                })?;
            if let Some((uid, gid)) = owner {
                self.chown
                    .apply(file_name, Some(file.as_fd()), uid, gid)
                    .map_err(|errno| chown_failed(file_name, errno))?;
            }
            inputs.push(format!("{name}/{file_name}"));
        }
        if let Some((uid, gid)) = owner {
            self.chown
                .apply(&name, Some(dir.as_fd()), uid, gid)
                .map_err(|errno| chown_failed(&name, errno))?;
        }
        Ok(InstanceFiles {
            path,
            dir: Some(Arc::new(dir)),
            stdout: Box::new(stdout),
            stderr: Box::new(stderr),
            plan: backend_plan,
            inputs,
        })
    }

    fn instance_in(
        &self,
        plan: &InstancePlan,
        backend: &dyn Backend,
        workload: &Workload,
        identity: Option<BackendIdentity>,
        fs: &dyn Fs,
    ) -> Result<InstanceFiles, PrepareError> {
        let name = plan.name.dir_name();
        let path = self.path.join(&name);
        let create = |what: &str, source: io::Error| PrepareError::Create {
            instance: name.clone(),
            name: what.to_owned(),
            source,
        };
        fs.create_dir(&path)
            .map_err(|source| create(&name, source))?;
        let backend_plan = backend
            .plan(workload, &path, &plan.cpus)
            .map_err(PrepareError::Backend)?;
        if backend_plan.program.as_os_str().is_empty() {
            return Err(PrepareError::EmptyProgram);
        }
        let stdout = fs
            .create_new(&path.join("stdout.log"))
            .map_err(|source| create("stdout.log", source))?;
        let stderr = fs
            .create_new(&path.join("stderr.log"))
            .map_err(|source| create("stderr.log", source))?;
        let chown = |target: &str| {
            if let Some(identity) = identity {
                self.chown
                    .apply(
                        target,
                        None,
                        Uid::from_raw(identity.uid()),
                        Gid::from_raw(identity.gid()),
                    )
                    .map_err(|source| PrepareError::Chown {
                        instance: name.clone(),
                        name: target.to_owned(),
                        source: source.into(),
                    })?;
            }
            Ok::<_, PrepareError>(())
        };
        let mut inputs = Vec::with_capacity(backend_plan.inputs.len());
        for input in &backend_plan.inputs {
            let file_name = input.name.as_str();
            fs.create_new(&path.join(file_name))
                .and_then(|mut file| file.write_all(&input.contents))
                .map_err(|source| create(file_name, source))?;
            chown(file_name)?;
            inputs.push(format!("{name}/{file_name}"));
        }
        chown(&name)?;
        Ok(InstanceFiles {
            path,
            dir: None,
            stdout: Box::new(MemorySink(stdout)),
            stderr: Box::new(MemorySink(stderr)),
            plan: backend_plan,
            inputs,
        })
    }

    /// How the samples writer creates `samples.jsonl` in this container.
    pub(crate) fn samples(&self, durability: Durability) -> Open {
        let (dir, parent) = match &self.storage {
            ContainerStorage::Disk { dir, parent } => (dir, parent),
            ContainerStorage::Memory(fs) => {
                let (fs, path) = (Arc::clone(fs), self.path.join(togi_trialfiles::SAMPLES));
                return Box::new(move || {
                    let file = fs
                        .create_new(&path)
                        .map_err(crate::error::StorageError::Create)?;
                    Ok(Box::new(MemorySamples(file)) as Box<dyn crate::samples::Storage>)
                });
            }
        };
        match durability {
            Durability::Durable => create_file(dir, parent),
            Durability::Unsynced => create_file_unsynced(dir, parent),
        }
    }
}

struct MemorySamples(Box<dyn FsFile>);

impl crate::samples::Storage for MemorySamples {
    fn append(&mut self, line: &[u8]) -> io::Result<()> {
        self.0.write_all(line)
    }

    fn close(self: Box<Self>) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::InstanceName;
    use crate::testing::TestBackend;
    use crate::tests::harness::own_identity;
    use std::fs;
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt, symlink};
    use std::sync::Mutex;
    use test_case::test_case;
    use togi_machine::{CoreId, CpuId, Regime, pick_workload};

    fn id(text: &str) -> TrialId {
        TrialId::parse(text).unwrap()
    }

    fn work() -> InstancePlan {
        InstancePlan {
            name: InstanceName::Work,
            core: CoreId::new(0).unwrap(),
            cpus: vec![CpuId(0)],
        }
    }

    fn workload() -> Workload {
        pick_workload(Regime::R1, 0).clone()
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn real_fs_the_container_is_recreated_with_fixed_modes_when_scoped() {
        let tmp = crate::testing::tempdir();
        let root = tmp.path().join("trials");
        let container = recreate(&root, &id("t1"), Some(own_identity())).unwrap();
        assert_eq!(mode_of(&root), 0o711);
        assert_eq!(mode_of(&container.path), 0o711);
        let unscoped = recreate(&root, &id("t2"), None).unwrap();
        assert_eq!(mode_of(&unscoped.path), 0o755);
    }

    #[test]
    fn real_fs_a_symlinked_trials_directory_is_refused() {
        let tmp = crate::testing::tempdir();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        symlink(&real, tmp.path().join("trials")).unwrap();
        assert!(recreate(&tmp.path().join("trials"), &id("t1"), None).is_err());
        assert!(!real.join("t1").exists());
    }

    #[test]
    fn real_fs_a_trials_directory_others_can_write_is_refused_before_any_change() {
        let tmp = crate::testing::tempdir();
        let root = tmp.path().join("trials");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("t1")).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
        let error = recreate(&root, &id("t1"), None).unwrap_err();
        assert!(matches!(error, PrepareError::Untrusted { .. }), "{error}");
        assert!(root.join("t1").exists(), "nothing was removed");
        assert_eq!(mode_of(&root), 0o777, "nothing was chmodded");
    }

    #[test]
    fn real_fs_a_state_directory_others_can_write_is_refused() {
        let tmp = crate::testing::tempdir();
        let state = tmp.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o775)).unwrap();
        let error = recreate(&state.join("trials"), &id("t1"), None).unwrap_err();
        assert!(matches!(error, PrepareError::Untrusted { .. }), "{error}");
        assert!(!state.join("trials").exists());
    }

    #[test]
    fn real_fs_a_trials_directory_owned_by_another_user_is_refused() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let error = TrialsRoot::open(Path::new("/proc"), false).err().unwrap();
        assert!(matches!(error, PrepareError::Untrusted { .. }), "{error}");
    }

    #[test]
    fn real_fs_samples_are_read_only_from_regular_files_below_the_validated_root() {
        let tmp = crate::testing::tempdir();
        let root_path = tmp.path().join("trials");
        let container = recreate(&root_path, &id("t1"), None).unwrap();
        let root = TrialsRoot::open(&root_path, false).unwrap();
        assert!(root.samples(&id("t1")).is_none(), "missing is empty");
        let victim = tmp.path().join("victim.jsonl");
        fs::write(&victim, "{\"elapsed_ms\":1}\n").unwrap();
        symlink(&victim, container.path.join("samples.jsonl")).unwrap();
        assert!(
            root.samples(&id("t1")).is_none(),
            "a symlink is never followed"
        );
        fs::remove_file(container.path.join("samples.jsonl")).unwrap();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            container.path.join("samples.jsonl"),
            rustix::fs::FileType::Fifo,
            Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        assert!(
            root.samples(&id("t1")).is_none(),
            "a FIFO is never opened for reading"
        );
        fs::remove_file(container.path.join("samples.jsonl")).unwrap();
        fs::write(container.path.join("samples.jsonl"), "{\"elapsed_ms\":2}\n").unwrap();
        let mut text = String::new();
        root.samples(&id("t1"))
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "{\"elapsed_ms\":2}\n");
    }

    #[test]
    fn real_fs_an_old_trial_is_removed_without_following_symlinks() {
        let tmp = crate::testing::tempdir();
        let victim = tmp.path().join("victim");
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("keep.txt"), "keep").unwrap();
        let root = tmp.path().join("trials");
        let old = root.join("t1");
        fs::create_dir_all(old.join("work/nested")).unwrap();
        symlink(&victim, old.join("work/link")).unwrap();
        symlink(victim.join("keep.txt"), old.join("stdout.log")).unwrap();
        fs::write(old.join("work/nested/file"), "x").unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let container = recreate(&root, &id("t1"), None).unwrap();
        assert_eq!(fs::read_dir(&container.path).unwrap().count(), 0);
        assert_eq!(fs::read_to_string(victim.join("keep.txt")).unwrap(), "keep");
    }

    #[test]
    fn real_fs_a_trial_directory_that_is_a_symlink_is_replaced_not_followed() {
        let tmp = crate::testing::tempdir();
        let victim = tmp.path().join("victim");
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("keep.txt"), "keep").unwrap();
        let root = tmp.path().join("trials");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&victim, root.join("t1")).unwrap();
        let container = recreate(&root, &id("t1"), None).unwrap();
        assert!(fs::symlink_metadata(&container.path).unwrap().is_dir());
        assert_eq!(fs::read_to_string(victim.join("keep.txt")).unwrap(), "keep");
    }

    #[test]
    fn real_fs_a_relative_trials_directory_resolves_against_its_base_and_touches_nothing_else() {
        let tmp = crate::testing::tempdir();
        let root = TrialsRoot::open_from(
            tmp.path(),
            Path::new("state/../state/trials"),
            true,
            caller(),
        )
        .unwrap();
        assert_eq!(root.path, tmp.path().join("state/trials"));
        assert!(root.path.is_absolute() && root.path.is_dir());
        assert_eq!(mode_of(&tmp.path().join("state")), 0o755);
    }

    fn caller() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    #[test_case(0o777; "world writable")]
    #[test_case(0o775; "group writable")]
    #[test_case(0o757; "other writable")]
    fn real_fs_a_writable_ancestor_without_the_sticky_bit_is_refused_before_any_change(mode: u32) {
        let tmp = crate::testing::tempdir();
        let open = tmp.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(mode)).unwrap();
        let error = recreate(&open.join("state/trials"), &id("t1"), None).unwrap_err();
        assert!(
            matches!(error, PrepareError::Untrusted { ref path, reason } if *path == open && reason == "others can write to it"),
            "{error}"
        );
        assert!(!open.join("state").exists());
    }

    #[test]
    fn real_fs_a_symlinked_ancestor_is_refused_before_any_change() {
        let tmp = crate::testing::tempdir();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        symlink(&real, &link).unwrap();
        let error = recreate(&link.join("state/trials"), &id("t1"), None).unwrap_err();
        assert!(
            matches!(error, PrepareError::Directory { operation: "open", ref path, .. } if *path == link),
            "{error}"
        );
        assert!(!real.join("state").exists());
    }

    #[test]
    fn real_fs_a_sticky_world_writable_ancestor_the_runner_owns_is_accepted() {
        let tmp = crate::testing::tempdir();
        let shared = tmp.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();
        let container = recreate(&shared.join("state/trials"), &id("t1"), None).unwrap();
        assert!(container.path.is_dir());
    }

    #[test]
    fn real_fs_an_ancestor_another_user_owns_is_refused_whatever_sits_above_it() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let tmp = crate::testing::tempdir();
        let stranger = caller() + 1;
        let error = TrialsRoot::open_from(
            Path::new("/"),
            &tmp.path().join("state/trials"),
            false,
            stranger,
        )
        .err()
        .unwrap();
        let PrepareError::Untrusted { path, reason } = error else {
            panic!("not refused as untrusted: {error}");
        };
        assert_eq!(reason, "it belongs to another user");
        assert_eq!(fs::metadata(&path).unwrap().uid(), caller());
        assert!(tmp.path().starts_with(&path));
    }

    #[test_case(0o755, 0, Role::Ancestor, None; "root's plain ancestor")]
    #[test_case(0o755, 1000, Role::Ancestor, None; "the runner's plain ancestor")]
    #[test_case(0o755, 1001, Role::Ancestor, Some("it belongs to another user"); "a stranger's ancestor")]
    #[test_case(0o1777, 0, Role::Ancestor, None; "root's sticky world-writable ancestor")]
    #[test_case(0o1777, 1001, Role::Ancestor, Some("it belongs to another user"); "a stranger's entry below a sticky ancestor")]
    #[test_case(0o777, 0, Role::Ancestor, Some("others can write to it"); "writable without sticky")]
    #[test_case(0o775, 1000, Role::Ancestor, Some("others can write to it"); "group writable without sticky")]
    #[test_case(0o755, 1001, Role::NamespaceRoot, None; "the namespace root is trusted whoever owns it")]
    #[test_case(0o777, 0, Role::NamespaceRoot, Some("others can write to it"); "a writable namespace root")]
    #[test_case(0o755, 0, Role::State, None; "root's state directory")]
    #[test_case(0o1755, 1000, Role::State, None; "the runner's sticky state directory")]
    #[test_case(0o1777, 1000, Role::State, Some("others can write to it"); "sticky does not cover the state directory")]
    #[test_case(0o755, 1000, Role::Trials, None; "the runner's trials directory")]
    #[test_case(0o755, 0, Role::Trials, Some("it belongs to another user"); "root's trials directory is refused to the runner")]
    #[test_case(0o1777, 1000, Role::Trials, Some("others can write to it"); "sticky does not cover the trials directory")]
    fn each_directory_of_the_path_is_judged_by_its_place(
        mode: u32,
        owner: u32,
        role: Role,
        refused: Option<&'static str>,
    ) {
        assert_eq!(refusal(mode, owner, 1000, role), refused);
    }

    fn directory(uid: u32, gid: u32, mode: u32) -> Link {
        Link {
            path: PathBuf::from("/d"),
            uid,
            gid,
            mode,
        }
    }

    #[test_case(0, 2000, 0o710, true; "another user's directory, the account's primary group, group bit")]
    #[test_case(0, 2000, 0o701, false; "the other bit does not help a group member")]
    #[test_case(0, 3000, 0o701, true; "a stranger searches by the other bit")]
    #[test_case(0, 3000, 0o710, false; "the group bit does not help a stranger")]
    #[test_case(1000, 3000, 0o100, true; "the owner searches by the owner bit")]
    #[test_case(1000, 2000, 0o011, false; "the owner class overrides the group and other bits")]
    fn the_account_searches_by_exactly_one_permission_class(
        uid: u32,
        gid: u32,
        mode: u32,
        searchable_by_account: bool,
    ) {
        let identity = BackendIdentity::new(1000, 2000).unwrap();
        assert_eq!(
            searchable(&directory(uid, gid, mode), identity),
            searchable_by_account
        );
    }

    #[test]
    fn the_deepest_directory_the_account_cannot_search_is_the_one_named() {
        let identity = BackendIdentity::new(1000, 2000).unwrap();
        let at = |path: &str, mode| Link {
            path: PathBuf::from(path),
            ..directory(0, 2000, mode)
        };
        let chain = [
            at("/", 0o755),
            at("/a", 0o701),
            at("/a/b", 0o701),
            at("/a/b/c", 0o710),
        ];
        let error = check_search(&chain, identity).unwrap_err();
        assert!(
            matches!(error, PrepareError::Inaccessible { ref path, uid: 1000, gid: 2000 } if path == Path::new("/a/b")),
            "{error}"
        );
        assert!(check_search(&chain[..1], identity).is_ok());
    }

    #[test]
    fn real_fs_an_ancestor_the_account_cannot_search_is_refused_without_chmod() {
        let tmp = crate::testing::tempdir();
        let private = tmp.path().join("private");
        fs::create_dir(&private).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        let own = own_identity();
        let other =
            BackendIdentity::new(own.uid().wrapping_add(1), own.gid().wrapping_add(1)).unwrap();
        let error = recreate(&private.join("trials"), &id("t1"), Some(other)).unwrap_err();
        assert!(
            matches!(error, PrepareError::Inaccessible { ref path, .. } if *path == private.canonicalize().unwrap())
        );
        assert_eq!(mode_of(&private), 0o700);
    }

    #[test]
    fn real_fs_instances_get_exclusive_logs_inputs_and_fixed_modes() {
        let tmp = crate::testing::tempdir();
        let container =
            recreate(&tmp.path().join("trials"), &id("t1"), Some(own_identity())).unwrap();
        let mut backend = TestBackend::new("/bin/true");
        backend.inputs = vec![("prime.txt", "a"), ("local.txt", "b")];
        backend.watched = vec!["results.txt"];
        let files = container
            .instance(&work(), &backend, &workload(), Some(own_identity()))
            .unwrap();
        assert_eq!(files.inputs, ["work/prime.txt", "work/local.txt"]);
        assert_eq!(mode_of(&files.path), 0o755);
        assert_eq!(
            fs::read_to_string(files.path.join("prime.txt")).unwrap(),
            "a"
        );
        assert!(files.path.join("stdout.log").exists() && files.path.join("stderr.log").exists());
        assert_eq!(files.plan.watched.len(), 1);
    }

    fn recording(fail_at: Option<&'static str>) -> (Chown, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&calls);
        let chown = Chown::new(move |target, _, _, _| {
            seen.lock().unwrap().push(target.to_owned());
            if Some(target) == fail_at {
                Err(Errno::PERM)
            } else {
                Ok(())
            }
        });
        (chown, calls)
    }

    fn with_inputs() -> TestBackend {
        let mut backend = TestBackend::new("/bin/true");
        backend.inputs = vec![("prime.txt", "a"), ("local.txt", "b")];
        backend
    }

    fn memory_container(fs: &Arc<togi_journal::fs::MemFs>, trial: &str) -> Container {
        recreate_in(Path::new("/trials"), &id(trial), None, Some(fs.clone())).unwrap()
    }

    fn memory_identity() -> BackendIdentity {
        BackendIdentity::new(1000, 1000).unwrap()
    }

    #[test]
    fn the_inputs_and_then_the_directory_change_owner_and_only_when_scoped() {
        let fs = Arc::new(togi_journal::fs::MemFs::new());
        let mut container = memory_container(&fs, "t1");
        let (chown, calls) = recording(None);
        container.chown = chown;
        container
            .instance(
                &work(),
                &with_inputs(),
                &workload(),
                Some(memory_identity()),
            )
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), ["prime.txt", "local.txt", "work"]);
        let other = memory_container(&fs, "t2");
        let mut unscoped = other;
        let (chown, calls) = recording(None);
        unscoped.chown = chown;
        unscoped
            .instance(&work(), &with_inputs(), &workload(), None)
            .unwrap();
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failed_input_ownership_names_the_input_and_changes_nothing_after_it() {
        let fs = Arc::new(togi_journal::fs::MemFs::new());
        let mut container = memory_container(&fs, "t1");
        let (chown, calls) = recording(Some("prime.txt"));
        container.chown = chown;
        let error = container
            .instance(
                &work(),
                &with_inputs(),
                &workload(),
                Some(memory_identity()),
            )
            .unwrap_err();
        match error {
            PrepareError::Chown {
                ref instance,
                ref name,
                ref source,
            } => {
                assert_eq!((instance.as_str(), name.as_str()), ("work", "prime.txt"));
                assert_eq!(source.raw_os_error(), Some(Errno::PERM.raw_os_error()));
            }
            other => panic!("not an ownership failure: {other}"),
        }
        assert_eq!(*calls.lock().unwrap(), ["prime.txt"]);
    }

    #[test]
    fn a_failed_directory_ownership_is_named_once_every_input_was_handed_over() {
        let fs = Arc::new(togi_journal::fs::MemFs::new());
        let mut container = memory_container(&fs, "t1");
        let (chown, calls) = recording(Some("work"));
        container.chown = chown;
        let error = container
            .instance(
                &work(),
                &with_inputs(),
                &workload(),
                Some(memory_identity()),
            )
            .unwrap_err();
        assert!(
            matches!(error, PrepareError::Chown { ref instance, ref name, .. } if instance == "work" && name == "work"),
            "{error}"
        );
        assert_eq!(*calls.lock().unwrap(), ["prime.txt", "local.txt", "work"]);
    }

    #[test]
    fn real_fs_a_second_instance_of_the_same_name_cannot_reuse_the_directory() {
        let tmp = crate::testing::tempdir();
        let container = recreate(&tmp.path().join("trials"), &id("t1"), None).unwrap();
        let backend = TestBackend::new("/bin/true");
        container
            .instance(&work(), &backend, &workload(), None)
            .unwrap();
        let error = container
            .instance(&work(), &backend, &workload(), None)
            .unwrap_err();
        assert!(matches!(error, PrepareError::Create { .. }));
    }

    #[test]
    fn real_fs_an_input_named_like_a_log_or_repeated_fails_exclusively() {
        let tmp = crate::testing::tempdir();
        let container = recreate(&tmp.path().join("trials"), &id("t1"), None).unwrap();
        let mut backend = TestBackend::new("/bin/true");
        backend.inputs = vec![("stdout.log", "x")];
        let error = container
            .instance(&work(), &backend, &workload(), None)
            .unwrap_err();
        assert!(matches!(error, PrepareError::Create { ref name, .. } if name == "stdout.log"));
        let victim = tmp.path().join("victim");
        fs::write(&victim, "keep").unwrap();
        let other = recreate(&tmp.path().join("trials"), &id("t2"), None).unwrap();
        let mut repeated = TestBackend::new("/bin/true");
        repeated.inputs = vec![("a", "1"), ("a", "2")];
        let error = other
            .instance(&work(), &repeated, &workload(), None)
            .unwrap_err();
        assert!(matches!(error, PrepareError::Create { ref name, .. } if name == "a"));
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
    }

    #[test]
    fn backend_failures_and_an_empty_program_stop_before_any_log_exists() {
        let fs = Arc::new(togi_journal::fs::MemFs::new());
        let container = memory_container(&fs, "t1");
        let mut failing = TestBackend::new("/bin/true");
        failing.fail_plan = true;
        let error = container
            .instance(&work(), &failing, &workload(), None)
            .unwrap_err();
        assert!(matches!(error, PrepareError::Backend(_)));
        let empty = TestBackend::new("");
        let other = memory_container(&fs, "t2");
        let error = other
            .instance(&work(), &empty, &workload(), None)
            .unwrap_err();
        assert!(matches!(error, PrepareError::EmptyProgram));
        assert!(
            fs.kind(&other.path.join("work/stdout.log"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn real_fs_retained_log_descriptors_survive_a_path_replacement() {
        let tmp = crate::testing::tempdir();
        let container = recreate(&tmp.path().join("trials"), &id("t1"), None).unwrap();
        let backend = TestBackend::new("/bin/true");
        let mut files = container
            .instance(&work(), &backend, &workload(), None)
            .unwrap();
        let victim = tmp.path().join("victim");
        fs::write(&victim, "keep").unwrap();
        fs::rename(
            files.path.join("stdout.log"),
            files.path.join("stdout.log.retained"),
        )
        .unwrap();
        symlink(&victim, files.path.join("stdout.log")).unwrap();
        files.stdout.write_all(b"output\n").unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(
            fs::read_to_string(files.path.join("stdout.log.retained")).unwrap(),
            "output\n"
        );
    }

    #[test]
    fn real_fs_both_retained_logs_survive_a_symlink_swap_of_their_paths() {
        let tmp = crate::testing::tempdir();
        let container = recreate(&tmp.path().join("trials"), &id("t1"), None).unwrap();
        let mut files = container
            .instance(&work(), &TestBackend::new("/bin/true"), &workload(), None)
            .unwrap();
        for (log, text) in [("stdout.log", "out\n"), ("stderr.log", "err\n")] {
            let victim = tmp.path().join(format!("victim-{log}"));
            fs::write(&victim, "protected journal\n").unwrap();
            fs::rename(
                files.path.join(log),
                files.path.join(format!("{log}.retained")),
            )
            .unwrap();
            symlink(&victim, files.path.join(log)).unwrap();
            let file = if log == "stdout.log" {
                &mut files.stdout
            } else {
                &mut files.stderr
            };
            file.write_all(text.as_bytes()).unwrap();
            assert_eq!(fs::read_to_string(&victim).unwrap(), "protected journal\n");
            assert_eq!(
                fs::read_to_string(files.path.join(format!("{log}.retained"))).unwrap(),
                text
            );
        }
    }

    #[test_case(false; "a work directory holding the symlinks")]
    #[test_case(true; "a work directory that is itself a symlink")]
    fn real_fs_a_reused_trial_directory_never_writes_through_old_input_or_log_symlinks(
        linked: bool,
    ) {
        let tmp = crate::testing::tempdir();
        let victim = tmp.path().join("control");
        fs::write(&victim, "protected control\n").unwrap();
        let root = tmp.path().join("trials");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let old = root.join("t1");
        fs::create_dir(&old).unwrap();
        let planted = if linked {
            let target = tmp.path().join("attacker");
            fs::create_dir(&target).unwrap();
            symlink(&target, old.join("work")).unwrap();
            target
        } else {
            fs::create_dir(old.join("work")).unwrap();
            old.join("work")
        };
        for name in ["prime.txt", "local.txt", "stdout.log", "stderr.log"] {
            symlink(&victim, planted.join(name)).unwrap();
        }
        let container = recreate(&root, &id("t1"), Some(own_identity())).unwrap();
        let files = container
            .instance(&work(), &with_inputs(), &workload(), Some(own_identity()))
            .unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "protected control\n");
        for (name, text) in [("prime.txt", "a"), ("local.txt", "b")] {
            let path = files.path.join(name);
            assert!(fs::symlink_metadata(&path).unwrap().is_file(), "{name}");
            assert_eq!(fs::read_to_string(path).unwrap(), text);
        }
        for name in ["stdout.log", "stderr.log"] {
            assert!(
                fs::symlink_metadata(files.path.join(name))
                    .unwrap()
                    .is_file(),
                "{name}"
            );
        }
        if linked {
            assert_eq!(fs::read_dir(&planted).unwrap().count(), 4);
        }
    }
}
