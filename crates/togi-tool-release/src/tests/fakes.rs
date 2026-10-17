//! In-memory stand-ins for git, the GitHub API, the clock and the release gates.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{self, Cursor, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::gates::{CheckError, Clock, Gates, QueueError, require_no_hardware_wait};
use crate::git::{Git, GitCommand, GitError};
use crate::github::{GitHub, Repository, Request, Response, Transport};

/// One recorded git command.
#[derive(Clone, Debug)]
pub(crate) struct Call {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stdin: String,
}

impl Call {
    pub(crate) fn command(&self) -> String {
        self.args.join(" ")
    }
}

/// A repository answering each command line from `responses`; it stores `hash-object` input as numbered blobs,
/// accepts every `update-index` and fails every command equal to a `failures` entry or starting with it and a
/// space.
#[derive(Default)]
pub(crate) struct FakeGit {
    pub responses: HashMap<String, String>,
    pub failures: Vec<String>,
    pub blobs: Vec<String>,
    pub calls: Vec<Call>,
    pub removed: Vec<String>,
}

/// The error a failing git command reports, with `injected` as its standard error.
pub(crate) fn injected(command: &str) -> GitError {
    GitError::Failed {
        command: command.to_owned(),
        status: "exit status 1".to_owned(),
        stderr: "injected".to_owned(),
    }
}

impl FakeGit {
    pub(crate) fn respond(&mut self, command: &str, output: &str) {
        self.responses.insert(command.to_owned(), output.to_owned());
    }

    pub(crate) fn commands(&self) -> Vec<String> {
        self.calls.iter().map(Call::command).collect()
    }

    /// The input of every run of `command`.
    pub(crate) fn stdin(&self, command: &str) -> Vec<String> {
        self.calls
            .iter()
            .filter(|c| c.command() == command)
            .map(|c| c.stdin.clone())
            .collect()
    }

    /// The commands that write objects, an index or the checkout.
    pub(crate) fn mutations(&self) -> Vec<String> {
        self.calls
            .iter()
            .filter(|c| {
                matches!(
                    c.args[0].as_str(),
                    "read-tree"
                        | "hash-object"
                        | "update-index"
                        | "write-tree"
                        | "commit-tree"
                        | "checkout"
                )
            })
            .map(Call::command)
            .collect()
    }

    /// The mode and content each `update-index --cacheinfo` stages by path, and the paths removed.
    pub(crate) fn staged(&self) -> (HashMap<String, (String, String)>, Vec<String>) {
        let mut files = HashMap::new();
        let mut removed = Vec::new();
        for call in self.calls.iter().filter(|c| c.args[0] == "update-index") {
            match call.args[1].as_str() {
                "--cacheinfo" => {
                    let mut parts = call.args[2].splitn(3, ',');
                    let (mode, blob, path) = (
                        parts.next().unwrap_or_default(),
                        parts.next().unwrap_or_default(),
                        parts.next().unwrap_or_default(),
                    );
                    let index: usize = blob.trim_start_matches("blob").parse().unwrap();
                    files.insert(
                        path.to_owned(),
                        (mode.to_owned(), self.blobs[index].clone()),
                    );
                }
                "--force-remove" => removed.push(call.args[2].clone()),
                other => panic!("unexpected update-index {other}"),
            }
        }
        (files, removed)
    }
}

impl Git for FakeGit {
    fn run(&mut self, command: GitCommand<'_>) -> Result<String, GitError> {
        let call = Call {
            args: command.args.iter().map(ToString::to_string).collect(),
            env: command
                .env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            stdin: command.stdin.to_owned(),
        };
        let key = call.command();
        self.calls.push(call);
        if self
            .failures
            .iter()
            .any(|prefix| key == *prefix || key.starts_with(&format!("{prefix} ")))
        {
            return Err(injected(&key));
        }
        if key == "hash-object -w --stdin" {
            self.blobs.push(command.stdin.to_owned());
            return Ok(format!("blob{}\n", self.blobs.len() - 1));
        }
        if command.args[0] == "update-index" {
            return Ok(String::new());
        }
        match self.responses.get(&key) {
            Some(output) => Ok(output.clone()),
            None => panic!("unexpected git {key}"),
        }
    }

    fn remove_scratch(&mut self, path: &str) {
        self.removed.push(path.to_owned());
    }
}

/// The base URL of the fake API.
pub(crate) const API: &str = "https://api.forge.example";

/// A response with `status` and `body`, as a fake API handler returns it.
#[expect(
    clippy::unnecessary_wraps,
    reason = "handlers return the transport's result"
)]
pub(crate) fn respond(status: u16, body: &str) -> io::Result<Response> {
    Ok(Response {
        status,
        body: Box::new(Cursor::new(body.to_owned().into_bytes())),
    })
}

/// `METHOD /path?query` of a request to the fake API.
pub(crate) fn uri(request: &Request) -> String {
    format!(
        "{} {}",
        request.method,
        request.url.strip_prefix(API).unwrap_or(&request.url)
    )
}

type Handler = Box<dyn FnMut(&Request) -> io::Result<Response>>;

/// Answers requests with a handler and records them.
struct FakeTransport {
    requests: Rc<RefCell<Vec<Request>>>,
    handler: Handler,
}

impl Transport for FakeTransport {
    fn send(&mut self, request: Request) -> io::Result<Response> {
        let response = (self.handler)(&request);
        self.requests.borrow_mut().push(request);
        response
    }
}

/// The requests a fake API received.
pub(crate) type Requests = Rc<RefCell<Vec<Request>>>;

/// A client of a fake API that `handler` answers, with token `test-token`, and the requests it receives.
pub(crate) fn fake_api(
    handler: impl FnMut(&Request) -> io::Result<Response> + 'static,
) -> (GitHub, Requests) {
    let requests = Requests::default();
    let transport = FakeTransport {
        requests: Rc::clone(&requests),
        handler: Box::new(handler),
    };
    (
        GitHub::new(API, "test-token", Box::new(transport)),
        requests,
    )
}

/// The `METHOD /path?query` of each request received.
pub(crate) fn uris(requests: &Requests) -> Vec<String> {
    requests.borrow().iter().map(uri).collect()
}

/// The repository `o/r`.
pub(crate) fn repository() -> Repository {
    Repository::parse("o/r").unwrap()
}

/// A clock that only sleeps advance, besides whoever shares `now`.
pub(crate) struct FakeClock {
    pub now: Rc<Cell<Instant>>,
    pub sleeps: Vec<Duration>,
}

impl FakeClock {
    pub(crate) fn new() -> Self {
        Self {
            now: Rc::new(Cell::new(Instant::now())),
            sleeps: Vec::new(),
        }
    }
}

impl Clock for FakeClock {
    fn now(&mut self) -> Instant {
        self.now.get()
    }

    fn sleep(&mut self, duration: Duration) {
        self.sleeps.push(duration);
        self.now.set(self.now.get() + duration);
    }
}

/// Gates that record what they were asked. The queue is empty unless `queue` names an API to read it from;
/// the check passes unless `fail_check` is set.
#[derive(Default)]
pub(crate) struct FakeGates {
    pub queue: Option<GitHub>,
    pub queue_reads: usize,
    pub checked: Vec<String>,
    pub fail_check: bool,
}

impl Gates for FakeGates {
    fn require_no_hardware_wait(&mut self) -> Result<(), QueueError> {
        self.queue_reads += 1;
        match &mut self.queue {
            Some(api) => require_no_hardware_wait(api, &repository()),
            None => Ok(()),
        }
    }

    fn require_green(&mut self, commit: &str, _: &mut dyn Write) -> Result<(), CheckError> {
        self.checked.push(commit.to_owned());
        if self.fail_check {
            return Err(CheckError::Concluded {
                url: "u".to_owned(),
                commit: commit.to_owned(),
                conclusion: "failure".to_owned(),
            });
        }
        Ok(())
    }
}
