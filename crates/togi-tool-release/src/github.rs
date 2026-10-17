//! The GitHub REST API, through a transport seam: the release workflow's token reads check runs and issues and
//! creates the GitHub Release.

use std::fmt::{self, Write as _};
use std::io::{self, Read};

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

/// An HTTP response whose body is still to be read.
pub struct Response {
    pub status: u16,
    pub body: Box<dyn Read>,
}

/// Sends HTTP requests. A non-2xx status is a response, not an error.
pub trait Transport {
    fn send(&mut self, request: Request) -> io::Result<Response>;
}

/// The network transport: the platform's TLS library and trusted roots. It follows no redirect, so the token
/// reaches only the API base the workflow names.
#[derive(Clone, Copy, Debug, Default)]
pub struct HttpTransport;

impl Transport for HttpTransport {
    fn send(&mut self, request: Request) -> io::Result<Response> {
        let method = match request.method {
            Method::Get => minreq::Method::Get,
            Method::Post => minreq::Method::Post,
        };
        let mut sent = minreq::Request::new(method, request.url)
            .with_headers(request.headers)
            .with_follow_redirects(false);
        if let Some(body) = request.body {
            sent = sent.with_body(body);
        }
        let response = sent.send_lazy().map_err(io::Error::other)?;
        Ok(Response {
            status: response.status_code,
            body: Box::new(response),
        })
    }
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
    #[error("read {method} {path} response")]
    Read {
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

/// A GitHub API client for one base URL and token.
pub struct GitHub {
    base: String,
    token: String,
    transport: Box<dyn Transport>,
}

impl GitHub {
    pub fn new(base: &str, token: &str, transport: Box<dyn Transport>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
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
            ("Authorization", format!("Bearer {}", self.token)),
            ("Accept", "application/vnd.github+json".to_owned()),
            ("X-GitHub-Api-Version", "2022-11-28".to_owned()),
            ("User-Agent", "togi-tool-release".to_owned()),
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
        let mut response = self
            .transport
            .send(request)
            .map_err(|source| ApiError::Send {
                method,
                path: path.to_owned(),
                source,
            })?;
        let mut data = Vec::new();
        response
            .body
            .read_to_end(&mut data)
            .map_err(|source| ApiError::Read {
                method,
                path: path.to_owned(),
                source,
            })?;
        let status = response.status;
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

/// The API client and repository the release workflow's `GITHUB_API_URL`, `GITHUB_REPOSITORY` and
/// `GITHUB_TOKEN` name, `lookup` reading each variable.
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
    let token = read("GITHUB_TOKEN")?;
    let repository = Repository::parse(&repository)?;
    Ok((GitHub::new(&base, &token, transport), repository))
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
