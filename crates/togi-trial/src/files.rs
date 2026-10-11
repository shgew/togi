use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustix::fs::{AtFlags, Dir, Gid, Mode, OFlags, Uid, fchmod, fchown, mkdirat, openat, unlinkat};
use rustix::io::Errno;
use togi_backend::{Backend, Plan};
use togi_machine::Workload;

use crate::id::TrialId;
use crate::identity::BackendIdentity;
use crate::plan::InstancePlan;
use crate::samples::{Open, create_file};

const MAX_DEPTH: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("{operation} {}", path.display())]
    Directory {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not a directory", path.display())]
    NotDirectory { path: PathBuf },
    #[error("{} is not searchable by uid {uid} gid {gid}", path.display())]
    Inaccessible { path: PathBuf, uid: u32, gid: u32 },
    #[error("create {name} in instance {instance}")]
    Create {
        instance: String,
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("hand {name} of instance {instance} to the backend account")]
    Chown {
        instance: String,
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("plan the backend instance")]
    Backend(#[source] togi_backend::Error),
    #[error("the backend plan names no program")]
    EmptyProgram,
}

/// The trial's root-owned directory, held by descriptor so nothing below is reached by path.
#[derive(Debug)]
pub(crate) struct Container {
    pub(crate) path: PathBuf,
    dir: Arc<OwnedFd>,
    parent: Arc<OwnedFd>,
}

/// One instance's directory, its retained logs and what the backend asked for.
#[derive(Debug)]
pub(crate) struct InstanceFiles {
    pub(crate) path: PathBuf,
    pub(crate) dir: Arc<OwnedFd>,
    pub(crate) stdout: File,
    pub(crate) stderr: File,
    pub(crate) plan: Plan,
    /// Generated input paths relative to the trial directory.
    pub(crate) inputs: Vec<String>,
}

fn directory_error(operation: &'static str, path: &Path, source: impl Into<io::Error>) -> PrepareError {
    PrepareError::Directory {
        operation,
        path: path.to_owned(),
        source: source.into(),
    }
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(io::Error::other)
}

fn open_directory(parent: Option<BorrowedFd<'_>>, path: &Path) -> Result<OwnedFd, Errno> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match parent {
        Some(parent) => openat(parent, path, flags, Mode::empty()),
        None => rustix::fs::open(path, flags & !OFlags::NOFOLLOW, Mode::empty()),
    }
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
    let child = openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
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

fn searchable(meta: &std::fs::Metadata, identity: BackendIdentity) -> bool {
    let bit = if meta.uid() == identity.uid() {
        0o100
    } else if meta.gid() == identity.gid() {
        0o010
    } else {
        0o001
    };
    meta.mode() & bit != 0
}

/// Every ancestor of `path`, resolved once, must be a directory the account can search.
fn check_ancestors(path: &Path, identity: BackendIdentity) -> Result<(), PrepareError> {
    let resolved =
        std::fs::canonicalize(path).map_err(|error| directory_error("resolve", path, error))?;
    for ancestor in resolved.ancestors() {
        let meta = std::fs::metadata(ancestor)
            .map_err(|error| directory_error("stat", ancestor, error))?;
        if !meta.is_dir() {
            return Err(PrepareError::NotDirectory {
                path: ancestor.to_owned(),
            });
        }
        if !searchable(&meta, identity) {
            return Err(PrepareError::Inaccessible {
                path: ancestor.to_owned(),
                uid: identity.uid(),
                gid: identity.gid(),
            });
        }
    }
    Ok(())
}

/// Removes any earlier trial of this id without following symlinks, recreates it, fixes its mode
/// (0711 when scoped, 0755 otherwise) by fchmod, and checks that the backend account can search
/// down to it.
pub(crate) fn recreate(
    root: &Path,
    id: &TrialId,
    identity: Option<BackendIdentity>,
) -> Result<Container, PrepareError> {
    let root = std::path::absolute(root).map_err(|error| directory_error("resolve", root, error))?;
    std::fs::create_dir_all(&root).map_err(|error| directory_error("create", &root, error))?;
    let parent = open_directory(None, &root).map_err(|error| directory_error("open", &root, error))?;
    let mode = Mode::from_raw_mode(if identity.is_some() { 0o711 } else { 0o755 });
    fchmod(&parent, mode).map_err(|error| directory_error("set the mode of", &root, error))?;
    let path = root.join(id.as_str());
    let name = c_name(OsStr::new(id.as_str())).map_err(|error| directory_error("name", &path, error))?;
    remove_entry(parent.as_fd(), &name, 0)
        .map_err(|error| directory_error("remove the earlier trial", &path, error))?;
    mkdirat(&parent, &name, Mode::from_raw_mode(0o755))
        .map_err(|error| directory_error("create", &path, error))?;
    let dir = open_directory(Some(parent.as_fd()), Path::new(id.as_str()))
        .map_err(|error| directory_error("open", &path, error))?;
    fchmod(&dir, mode).map_err(|error| directory_error("set the mode of", &path, error))?;
    if let Some(identity) = identity {
        check_ancestors(&path, identity)?;
    }
    Ok(Container {
        path,
        dir: Arc::new(dir),
        parent: Arc::new(parent),
    })
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
        let name = plan.name.dir_name();
        let path = self.path.join(&name);
        let create = |what: &str, source: Errno| PrepareError::Create {
            instance: name.clone(),
            name: what.to_owned(),
            source: source.into(),
        };
        mkdirat(&*self.dir, name.as_str(), Mode::from_raw_mode(0o755))
            .map_err(|errno| create(&name, errno))?;
        let dir = open_directory(Some(self.dir.as_fd()), Path::new(&name))
            .map_err(|errno| create(&name, errno))?;
        fchmod(&dir, Mode::from_raw_mode(0o755)).map_err(|errno| create(&name, errno))?;
        let backend_plan = backend
            .plan(workload, &path, &plan.cpus)
            .map_err(PrepareError::Backend)?;
        if backend_plan.program.as_os_str().is_empty() {
            return Err(PrepareError::EmptyProgram);
        }
        let stdout = File::from(
            create_exclusive(dir.as_fd(), "stdout.log").map_err(|errno| create("stdout.log", errno))?,
        );
        let stderr = File::from(
            create_exclusive(dir.as_fd(), "stderr.log").map_err(|errno| create("stderr.log", errno))?,
        );
        let owner = identity.map(|identity| {
            (
                Uid::from_raw(identity.uid()),
                Gid::from_raw(identity.gid()),
            )
        });
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
                fchown(&file, Some(uid), Some(gid)).map_err(|errno| PrepareError::Chown {
                    instance: name.clone(),
                    name: file_name.to_owned(),
                    source: errno.into(),
                })?;
            }
            inputs.push(format!("{name}/{file_name}"));
        }
        if let Some((uid, gid)) = owner {
            fchown(&dir, Some(uid), Some(gid)).map_err(|errno| PrepareError::Chown {
                instance: name.clone(),
                name: name.clone(),
                source: errno.into(),
            })?;
        }
        Ok(InstanceFiles {
            path,
            dir: Arc::new(dir),
            stdout,
            stderr,
            plan: backend_plan,
            inputs,
        })
    }

    /// How the samples writer creates `samples.jsonl` in this container.
    pub(crate) fn samples(&self) -> Open {
        create_file(&self.dir, &self.parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::InstanceName;
    use crate::testing::TestBackend;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use togi_machine::{CoreId, CpuId, Regime, pick_workload};

    fn own_identity() -> BackendIdentity {
        let meta = fs::metadata(".").unwrap();
        BackendIdentity::new(meta.uid(), meta.gid()).unwrap()
    }

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
    fn the_container_is_recreated_with_fixed_modes_when_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("trials");
        let container = recreate(&root, &id("t1"), Some(own_identity())).unwrap();
        assert_eq!(mode_of(&root), 0o711);
        assert_eq!(mode_of(&container.path), 0o711);
        let unscoped = recreate(&root, &id("t2"), None).unwrap();
        assert_eq!(mode_of(&unscoped.path), 0o755);
    }

    #[test]
    fn an_old_trial_is_removed_without_following_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let victim = tmp.path().join("victim");
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("keep.txt"), "keep").unwrap();
        let root = tmp.path().join("trials");
        let old = root.join("t1");
        fs::create_dir_all(old.join("work/nested")).unwrap();
        symlink(&victim, old.join("work/link")).unwrap();
        symlink(victim.join("keep.txt"), old.join("stdout.log")).unwrap();
        fs::write(old.join("work/nested/file"), "x").unwrap();
        let container = recreate(&root, &id("t1"), None).unwrap();
        assert_eq!(fs::read_dir(&container.path).unwrap().count(), 0);
        assert_eq!(fs::read_to_string(victim.join("keep.txt")).unwrap(), "keep");
    }

    #[test]
    fn a_trial_directory_that_is_a_symlink_is_replaced_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let victim = tmp.path().join("victim");
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("keep.txt"), "keep").unwrap();
        let root = tmp.path().join("trials");
        fs::create_dir(&root).unwrap();
        symlink(&victim, root.join("t1")).unwrap();
        let container = recreate(&root, &id("t1"), None).unwrap();
        assert!(fs::symlink_metadata(&container.path).unwrap().is_dir());
        assert_eq!(fs::read_to_string(victim.join("keep.txt")).unwrap(), "keep");
    }

    #[test]
    fn a_relative_trial_directory_resolves_to_an_absolute_path() {
        let relative = PathBuf::from(format!("trial-test-relative-{}", std::process::id()));
        let container = recreate(&relative, &id("t1"), None).unwrap();
        assert!(container.path.is_absolute());
        assert!(container.path.starts_with(std::env::current_dir().unwrap()));
        fs::remove_dir_all(container.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn an_ancestor_the_account_cannot_search_is_refused_without_chmod() {
        let tmp = tempfile::tempdir().unwrap();
        let private = tmp.path().join("private");
        fs::create_dir(&private).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        let own = own_identity();
        let other = BackendIdentity::new(own.uid().wrapping_add(1), own.gid().wrapping_add(1)).unwrap();
        let error = recreate(&private.join("trials"), &id("t1"), Some(other)).unwrap_err();
        assert!(matches!(error, PrepareError::Inaccessible { ref path, .. } if *path == private.canonicalize().unwrap()));
        assert_eq!(mode_of(&private), 0o700);
    }

    #[test]
    fn instances_get_exclusive_logs_inputs_and_fixed_modes() {
        let tmp = tempfile::tempdir().unwrap();
        let container = recreate(&tmp.path().join("trials"), &id("t1"), Some(own_identity())).unwrap();
        let mut backend = TestBackend::new("/bin/true");
        backend.inputs = vec![("prime.txt", "a"), ("local.txt", "b")];
        backend.watched = vec!["results.txt"];
        let files = container
            .instance(&work(), &backend, &workload(), Some(own_identity()))
            .unwrap();
        assert_eq!(files.inputs, ["work/prime.txt", "work/local.txt"]);
        assert_eq!(mode_of(&files.path), 0o755);
        assert_eq!(fs::read_to_string(files.path.join("prime.txt")).unwrap(), "a");
        assert!(files.path.join("stdout.log").exists() && files.path.join("stderr.log").exists());
        assert_eq!(files.plan.watched.len(), 1);
    }

    #[test]
    fn a_second_instance_of_the_same_name_cannot_reuse_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let container = recreate(&tmp.path().join("trials"), &id("t1"), None).unwrap();
        let backend = TestBackend::new("/bin/true");
        container.instance(&work(), &backend, &workload(), None).unwrap();
        let error = container
            .instance(&work(), &backend, &workload(), None)
            .unwrap_err();
        assert!(matches!(error, PrepareError::Create { .. }));
    }

    #[test]
    fn an_input_named_like_a_log_or_repeated_fails_exclusively() {
        let tmp = tempfile::tempdir().unwrap();
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
        let error = other.instance(&work(), &repeated, &workload(), None).unwrap_err();
        assert!(matches!(error, PrepareError::Create { ref name, .. } if name == "a"));
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
    }

    #[test]
    fn backend_failures_and_an_empty_program_stop_before_any_log_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let container = recreate(&tmp.path().join("trials"), &id("t1"), None).unwrap();
        let mut failing = TestBackend::new("/bin/true");
        failing.fail_plan = true;
        let error = container
            .instance(&work(), &failing, &workload(), None)
            .unwrap_err();
        assert!(matches!(error, PrepareError::Backend(_)));
        let empty = TestBackend::new("");
        let other = recreate(&tmp.path().join("trials"), &id("t2"), None).unwrap();
        let error = other.instance(&work(), &empty, &workload(), None).unwrap_err();
        assert!(matches!(error, PrepareError::EmptyProgram));
        assert!(!other.path.join("work/stdout.log").exists());
    }

    #[test]
    fn retained_log_descriptors_survive_a_path_replacement() {
        let tmp = tempfile::tempdir().unwrap();
        let container = recreate(&tmp.path().join("trials"), &id("t1"), None).unwrap();
        let backend = TestBackend::new("/bin/true");
        let mut files = container.instance(&work(), &backend, &workload(), None).unwrap();
        let victim = tmp.path().join("victim");
        fs::write(&victim, "keep").unwrap();
        fs::rename(files.path.join("stdout.log"), files.path.join("stdout.log.retained")).unwrap();
        symlink(&victim, files.path.join("stdout.log")).unwrap();
        files.stdout.write_all(b"output\n").unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(
            fs::read_to_string(files.path.join("stdout.log.retained")).unwrap(),
            "output\n"
        );
    }
}
