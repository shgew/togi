//! Stand-ins for gh and git, and the fixed commits and URLs the tests use.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use crate::process::Output;
use crate::{CommandError, Error, Programs, Tools};

pub(crate) const HEAD_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(crate) const MOVED_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";
pub(crate) const BASE_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub(crate) const OLD_HEAD_SHA: &str = "1111111111111111111111111111111111111111";
pub(crate) const OLD_BASE_SHA: &str = "2222222222222222222222222222222222222222";
pub(crate) const FIX_SHA: &str = "dddddddddddddddddddddddddddddddddddddddd";
pub(crate) const PR_URL: &str = "https://github.com/shgew/togi/pull/700";
pub(crate) const PREVIOUS_URL: &str = "https://github.com/shgew/togi/pull/700#issuecomment-4242";

pub(crate) type Handler = Box<dyn FnMut(&[String]) -> Output>;
/// Answers an isolated git call in a directory, given its arguments and its standard input (empty for a call
/// without input).
pub(crate) type IsolatedHandler = Box<dyn FnMut(&Path, &[String], &[u8]) -> Output>;

/// gh, git and the isolated git as handlers; every call is recorded, and a program without a handler
/// fails the test when called.
#[derive(Default)]
pub(crate) struct Fake {
    pub gh: Option<Handler>,
    pub git: Option<Handler>,
    pub isolated: Option<IsolatedHandler>,
    pub gh_calls: Vec<Vec<String>>,
    pub git_calls: Vec<Vec<String>>,
    pub isolated_calls: Vec<(PathBuf, Vec<String>)>,
}

fn owned(args: &[&str]) -> Vec<String> {
    args.iter().map(|&arg| arg.to_owned()).collect()
}

impl Programs for Fake {
    fn gh(&mut self, args: &[&str]) -> Output {
        let args = owned(args);
        self.gh_calls.push(args.clone());
        let Some(handle) = self.gh.as_mut() else {
            panic!("unexpected gh {args:?}");
        };
        handle(&args)
    }

    fn git(&mut self, args: &[&str]) -> Output {
        let args = owned(args);
        self.git_calls.push(args.clone());
        let Some(handle) = self.git.as_mut() else {
            panic!("unexpected git {args:?}");
        };
        handle(&args)
    }

    fn isolated_git(&mut self, dir: &Path, args: &[&str]) -> Output {
        self.isolated_git_input(dir, args, &[])
    }

    fn isolated_git_input(&mut self, dir: &Path, args: &[&str], input: &[u8]) -> Output {
        let args = owned(args);
        self.isolated_calls.push((dir.to_owned(), args.clone()));
        let Some(handle) = self.isolated.as_mut() else {
            panic!("unexpected isolated git {args:?} in {}", dir.display());
        };
        handle(dir, &args, input)
    }
}

impl Fake {
    pub(crate) fn new(gh: Option<Handler>, git: Option<Handler>) -> Self {
        Self {
            gh,
            git,
            ..Self::default()
        }
    }

    /// Each gh call as `gh <args>`.
    pub(crate) fn gh_joined(&self) -> Vec<String> {
        self.gh_calls
            .iter()
            .map(|args| format!("gh {}", args.join(" ")))
            .collect()
    }

    /// Each git call as `git <args>`.
    pub(crate) fn git_joined(&self) -> Vec<String> {
        self.git_calls
            .iter()
            .map(|args| format!("git {}", args.join(" ")))
            .collect()
    }

    /// The first argument of each isolated git call.
    pub(crate) fn isolated_subcommands(&self) -> Vec<String> {
        self.isolated_calls
            .iter()
            .map(|(_, args)| args[0].clone())
            .collect()
    }
}

/// A handler that answers every call with `out`.
pub(crate) fn answer(out: &'static str) -> Handler {
    Box::new(move |_| Ok(out.as_bytes().to_vec()))
}

pub(crate) fn fail(text: &str) -> CommandError {
    CommandError::new(text)
}

/// Fakes, a temporary directory and captured output, from which each test builds `Tools`.
pub(crate) struct Harness {
    pub fake: Fake,
    pub tmp: tempfile::TempDir,
    /// The directory the tools take as `TMPDIR`; the temporary directory unless a test points it elsewhere.
    pub tmp_path: String,
    pub gh_token: String,
    pub stdin: Cursor<Vec<u8>>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Harness {
    pub(crate) fn new(gh: Option<Handler>, git: Option<Handler>) -> Self {
        let tmp = tempfile::tempdir().expect("temporary directory");
        Self {
            fake: Fake::new(gh, git),
            tmp_path: tmp.path().to_string_lossy().into_owned(),
            tmp,
            gh_token: "token".to_owned(),
            stdin: Cursor::new(Vec::new()),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    pub(crate) fn tmp(&self) -> String {
        self.tmp_path.clone()
    }

    pub(crate) fn tools(&mut self) -> Tools<'_> {
        Tools {
            programs: &mut self.fake,
            gh_token: self.gh_token.clone(),
            tmp: self.tmp_path.clone(),
            stdin: &mut self.stdin,
            stdout: &mut self.stdout,
            stderr: &mut self.stderr,
        }
    }

    pub(crate) fn dispatch(&mut self, args: &[&str]) -> Result<(), Error> {
        crate::dispatch(&mut self.tools(), &owned(args))
    }

    pub(crate) fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub(crate) fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

pub(crate) fn read_file(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path.as_ref())
        .unwrap_or_else(|error| panic!("read {}: {error}", path.as_ref().display()))
}

/// The path of a fixture under the crate's `testdata/`.
pub(crate) fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(name)
}

/// Compares `got` with the fixture `name`; with `UPDATE_GOLDEN` set it rewrites the fixture first.
pub(crate) fn check_golden(name: &str, got: &str) {
    let path = testdata(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, got).expect("write golden");
    }
    let want = read_file(&path);
    assert_eq!(want, got, "{name} differs");
}
