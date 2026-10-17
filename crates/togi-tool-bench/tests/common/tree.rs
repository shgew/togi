use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use togi_journal::fs::{Fs, MemFs};
use togi_tool_bench::{Suite, SuiteSource};

#[derive(Clone)]
pub(crate) struct Tree {
    pub(crate) fs: Arc<MemFs>,
    root: PathBuf,
}

impl Tree {
    pub(crate) fn new() -> Self {
        Self::at("/bench")
    }

    pub(crate) fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            fs: Arc::new(MemFs::new()),
            root: root.into(),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.root
    }

    pub(crate) fn write(&self, path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) {
        self.fs.put(clean(path.as_ref()), bytes.as_ref().to_vec());
    }

    pub(crate) fn text(&self, path: impl AsRef<Path>) -> String {
        String::from_utf8(self.read(path.as_ref()).expect("fixture file")).expect("fixture UTF-8")
    }

    pub(crate) fn suite(&self) -> Suite {
        Suite::parse(
            include_bytes!("../../../../tools/bench/suite.json"),
            self.root.clone(),
        )
        .expect("committed suite")
    }

    pub(crate) fn committed() -> Self {
        Self::committed_at("/bench")
    }

    pub(crate) fn committed_at(root: impl Into<PathBuf>) -> Self {
        let tree = Self::at(root);
        macro_rules! machine {
            ($name:literal) => {
                tree.write(
                    tree.path().join(concat!("machines/", $name, ".json")),
                    include_bytes!(concat!("../../../../tools/bench/machines/", $name, ".json")),
                );
            };
        }
        machine!("flat-hazard");
        machine!("idle-limit");
        machine!("late-onset");
        machine!("misleading-mce");
        machine!("shared-voltage");
        machine!("target-nonmember-mce");
        machine!("target-shared-voltage");
        machine!("target-r7-vf-boost");
        machine!("target-r7-request-gap");
        machine!("target-fit-0");
        machine!("target-fit-1");
        machine!("target-fit-2");
        machine!("target-fit-3");
        machine!("target-fit-4");
        machine!("target-fit-5");
        machine!("target-fit-6");
        machine!("target-fit-7");
        machine!("target-fit-8");
        tree.write(
            tree.path().join("facts/target.jsonl.gz"),
            include_bytes!("../../../../tools/bench/facts/target.jsonl.gz"),
        );
        tree
    }
}

fn clean(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            component @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                clean.push(component.as_os_str());
            }
        }
    }
    clean
}

impl togi_sim::file::Source for Tree {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        String::from_utf8(self.read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let path = clean(path);
        self.fs
            .kind(&path)?
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        Ok(path)
    }
}

impl SuiteSource for Tree {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.fs.read(&clean(path))
    }
}
