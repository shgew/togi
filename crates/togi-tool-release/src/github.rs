//! The GitHub REST API, through a transport seam: the release workflow's token reads check runs and issues and
//! creates the GitHub Release. The network transport is `gh api`.

use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::io::{self, Write as _};
use std::process::{Command, Stdio};
use std::thread;

use serde::de::DeserializeOwned;

/// An HTTP method the release uses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    Get,
    Post,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Get => "GET",
            Self::Post => "POST",
        })
    }
}

/// One HTTP request, headers included.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Option<String>,
}

/// An HTTP response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Sends HTTP requests. A non-2xx status is a response, not an error.
pub trait Transport {
    fn send(&mut self, request: Request) -> io::Result<Response>;
}

/// The network transport: `gh api`, which authenticates with `GH_TOKEN`. gh attaches a token only to requests
/// for the host it belongs to, `GH_TOKEN` to github.com's API, and drops it on a redirect to another host, so the
/// token reaches GitHub's API alone whatever URL the request names.
#[derive(Clone, Debug)]
pub struct GhTransport {
    program: OsString,
}

impl GhTransport {
    /// A transport running `program` as gh.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// The command that sends `request`. It removes `CLICOLOR_FORCE` and `GH_FORCE_TTY` from gh's environment:
    /// either makes gh colour, pretty-print or page its output, which the tool parses.
    pub fn command(&self, request: &Request) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(gh_args(request))
            .env_remove("CLICOLOR_FORCE")
            .env_remove("GH_FORCE_TTY")
            .stdin(if request.body.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

impl Transport for GhTransport {
    #[expect(
        clippy::disallowed_methods,
        reason = "the release tool's one call site that runs gh"
    )]
    fn send(&mut self, request: Request) -> io::Result<Response> {
        let mut child = self
            .command(&request)
            .spawn()
            .map_err(|error| io::Error::new(error.kind(), format!("run gh: {error}")))?;
        let stdin = child.stdin.take();
        let body = request.body.as_deref().unwrap_or_default();
        let output = thread::scope(|scope| {
            let writer = scope.spawn(move || match stdin {
                Some(mut stdin) => stdin.write_all(body.as_bytes()),
                None => Ok(()),
            });
            let output = child.wait_with_output();
            let written = writer
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("the stdin writer panicked")));
            output.and_then(|output| {
                // gh may exit before reading its input; its status and output then tell what happened.
                match written {
                    Err(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(error),
                    Ok(()) | Err(_) => Ok(output),
                }
            })
        })?;
        let failed = || {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            io::Error::other(if stderr.is_empty() {
                format!("gh {}", output.status)
            } else {
                stderr
            })
        };
        match parse_response(&output.stdout) {
            Some(response) if output.status.success() || !(200..300).contains(&response.status) => {
                Ok(response)
            }
            _ => Err(failed()),
        }
    }
}

/// The `gh` arguments that send `request`: its headers, its body from standard input, and `--include`, which
/// prints the status line and headers before the body.
pub fn gh_args(request: &Request) -> Vec<String> {
    let mut args = vec![
        "api".to_owned(),
        "--include".to_owned(),
        "--method".to_owned(),
        request.method.to_string(),
    ];
    for (name, value) in &request.headers {
        args.push("--header".to_owned());
        args.push(format!("{name}: {value}"));
    }
    if request.body.is_some() {
        args.extend(["--input".to_owned(), "-".to_owned()]);
    }
    args.push(request.url.clone());
    args
}

/// The response `gh api --include` printed: a status line such as `HTTP/2.0 404 Not Found`, header lines, an
/// empty line and the body. `None` when the output does not start so, as when gh sent no request.
pub fn parse_response(output: &[u8]) -> Option<Response> {
    let mut rest = output;
    let mut status = None;
    loop {
        let end = rest.iter().position(|&b| b == b'\n')?;
        let line = &rest[..end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        rest = &rest[end + 1..];
        if line.is_empty() {
            break;
        }
        if status.is_none() {
            let line = std::str::from_utf8(line).ok()?;
            status = Some(
                line.strip_prefix("HTTP/")?
                    .split(' ')
                    .nth(1)?
                    .parse()
                    .ok()?,
            );
        }
    }
    Some(Response {
        status: status?,
        body: rest.to_vec(),
    })
}

/// Why a GitHub API request failed.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{method} {path}")]
    Send {
        method: Method,
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("{method} {path}: HTTP {status}: {body}")]
    Status {
        method: Method,
        path: String,
        status: u16,
        body: String,
    },
    #[error("decode {method} {path} response")]
    Decode {
        method: Method,
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

/// A GitHub API client for one base URL.
pub struct GitHub {
    base: String,
    transport: Box<dyn Transport>,
}

impl GitHub {
    pub fn new(base: &str, transport: Box<dyn Transport>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            transport,
        }
    }

    /// Sends a request for `path` and returns its status and body, failing on any status but 2xx and `allowed`.
    pub fn request(
        &mut self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
        allowed: &[u16],
    ) -> Result<(u16, Vec<u8>), ApiError> {
        let mut headers = vec![
            ("Accept", "application/vnd.github+json".to_owned()),
            ("X-GitHub-Api-Version", "2022-11-28".to_owned()),
        ];
        if body.is_some() {
            headers.push(("Content-Type", "application/json".to_owned()));
        }
        let request = Request {
            method,
            url: format!("{}{path}", self.base),
            headers,
            body: body.map(ToString::to_string),
        };
        let Response { status, body: data } =
            self.transport
                .send(request)
                .map_err(|source| ApiError::Send {
                    method,
                    path: path.to_owned(),
                    source,
                })?;
        if !allowed.contains(&status) && !(200..300).contains(&status) {
            return Err(ApiError::Status {
                method,
                path: path.to_owned(),
                status,
                body: String::from_utf8_lossy(&data).trim().to_owned(),
            });
        }
        Ok((status, data))
    }

    /// Sends a request for `path`, requires a 2xx status and decodes the body.
    pub fn decoded<T: DeserializeOwned>(
        &mut self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, ApiError> {
        let (_, data) = self.request(method, path, body, &[])?;
        serde_json::from_slice(&data).map_err(|source| ApiError::Decode {
            method,
            path: path.to_owned(),
            source,
        })
    }
}

/// The repository the workflow runs in, `owner/name`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Repository {
    owner: String,
    name: String,
}

impl Repository {
    /// Parses `owner/name`, each part of ASCII letters, digits, `_`, `.` and `-`, and neither `.` nor `..`.
    pub fn parse(text: &str) -> Result<Self, EnvError> {
        let invalid = || EnvError::Repository(text.to_owned());
        let (owner, name) = text.split_once('/').ok_or_else(invalid)?;
        let valid = |part: &str| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        };
        if !valid(owner) || !valid(name) {
            return Err(invalid());
        }
        Ok(Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// The API path of the repository, `/repos/owner/name`.
    pub fn path(&self) -> String {
        format!("/repos/{}/{}", escape(&self.owner), escape(&self.name))
    }
}

/// Why the workflow environment does not name a usable API.
#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("{0} is unset")]
    Unset(&'static str),
    #[error("invalid repository {0:?} (expected owner/name)")]
    Repository(String),
}

/// The API client and repository the release workflow's `GITHUB_API_URL` and `GITHUB_REPOSITORY` name, `lookup`
/// reading each variable. It refuses without `GH_TOKEN`, the token gh sends.
pub fn from_env(
    lookup: impl Fn(&'static str) -> Option<String>,
    transport: Box<dyn Transport>,
) -> Result<(GitHub, Repository), EnvError> {
    let read = |key| {
        lookup(key)
            .filter(|value| !value.is_empty())
            .ok_or(EnvError::Unset(key))
    };
    let base = read("GITHUB_API_URL")?;
    let repository = read("GITHUB_REPOSITORY")?;
    read("GH_TOKEN")?;
    let repository = Repository::parse(&repository)?;
    Ok((GitHub::new(&base, transport), repository))
}

/// `text` percent-encoded but for RFC 3986's unreserved characters, safe as a path segment or query value.
pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            escaped.push(char::from(b));
        } else {
            let _ = write!(escaped, "%{b:02X}");
        }
    }
    escaped
}

/// `pairs` as a query string, keys sorted.
pub fn query(pairs: &[(&str, &str)]) -> String {
    let mut pairs = pairs.to_vec();
    pairs.sort_unstable();
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", escape(key), escape(value)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
#[path = "tests/github.rs"]
mod tests;
