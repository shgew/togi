use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt as _;

use test_case::test_case;

use super::*;
use crate::chain;
use crate::fakes::{API, fake_api, respond};

#[test]
fn request_sends_version_and_body() {
    let (mut api, requests) = fake_api(|_| respond(201, r#"{"ok":"yes"}"#));
    let body = serde_json::json!({"name": "0.1.0"});
    let got: HashMap<String, String> = api.decoded(Method::Post, "/test", Some(&body)).unwrap();
    assert_eq!(got, HashMap::from([("ok".to_owned(), "yes".to_owned())]));
    assert_eq!(
        *requests.borrow(),
        [Request {
            method: Method::Post,
            url: format!("{API}/test"),
            headers: vec![
                ("Accept", "application/vnd.github+json".to_owned()),
                ("X-GitHub-Api-Version", "2022-11-28".to_owned()),
                ("Content-Type", "application/json".to_owned()),
            ],
            body: Some(r#"{"name":"0.1.0"}"#.to_owned()),
        }]
    );
}

fn post() -> Request {
    Request {
        method: Method::Post,
        url: format!("{API}/repos/o/r/releases"),
        headers: vec![("Accept", "application/vnd.github+json".to_owned())],
        body: Some("{}".to_owned()),
    }
}

#[test]
fn gh_args_send_method_headers_body_and_url() {
    assert_eq!(
        gh_args(&post()),
        [
            "api",
            "--include",
            "--method",
            "POST",
            "--header",
            "Accept: application/vnd.github+json",
            "--input",
            "-",
            "https://api.forge.example/repos/o/r/releases",
        ]
    );
    let get = Request {
        method: Method::Get,
        url: format!("{API}/x?page=2"),
        headers: Vec::new(),
        body: None,
    };
    assert_eq!(
        gh_args(&get),
        [
            "api",
            "--include",
            "--method",
            "GET",
            "https://api.forge.example/x?page=2"
        ]
    );
}

#[test_case(b"HTTP/2.0 200 OK\nContent-Type: application/json\r\nX: y\r\n\r\n{\"a\":1}\r\n\r\n", 200, b"{\"a\":1}\r\n\r\n" ; "headers")]
#[test_case(b"HTTP/1.1 404 Not Found\r\n\r\n{}", 404, b"{}" ; "no headers")]
#[test_case(b"HTTP/2.0 204 No Content\r\n\r\n", 204, b"" ; "empty body")]
fn parses_gh_response(output: &[u8], status: u16, body: &[u8]) {
    assert_eq!(
        parse_response(output),
        Some(Response {
            status,
            body: body.to_vec(),
        })
    );
}

#[test_case(b"" ; "empty")]
#[test_case(b"{\"a\":1}" ; "body only")]
#[test_case(b"HTTP/2.0 200 OK\r\nX: y\r\n" ; "unterminated headers")]
#[test_case(b"HTTP/2.0 OK\r\n\r\n" ; "no status code")]
#[test_case(b"gh: oops\n\n" ; "not a status line")]
fn refuses_other_gh_output(output: &[u8]) {
    assert_eq!(parse_response(output), None);
}

/// A gh stand-in in a temporary directory that records its arguments and standard input next to it and then
/// runs `script`.
fn stub_gh(script: &str) -> (tempfile::TempDir, GhTransport) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gh");
    fs::write(
        &path,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\ncat > \"$0.stdin\"\n{script}\n"),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, GhTransport::new(path))
}

#[test]
fn real_process_gh_transport_sends_body_and_reads_response() {
    let (dir, mut gh) = stub_gh(r#"printf 'HTTP/2.0 201 Created\r\nX: y\r\n\r\n{"ok":"yes"}'"#);
    assert_eq!(
        gh.send(post()).unwrap(),
        Response {
            status: 201,
            body: br#"{"ok":"yes"}"#.to_vec(),
        }
    );
    let args = fs::read_to_string(dir.path().join("gh.args")).unwrap();
    assert_eq!(args.lines().collect::<Vec<_>>(), gh_args(&post()));
    assert_eq!(
        fs::read_to_string(dir.path().join("gh.stdin")).unwrap(),
        "{}"
    );
}

#[test]
fn real_process_gh_transport_returns_error_statuses() {
    let (_dir, mut gh) = stub_gh(
        r"printf 'HTTP/2.0 404 Not Found\r\n\r\n{}'; echo 'gh: Not Found (HTTP 404)' >&2; exit 1",
    );
    assert_eq!(gh.send(post()).unwrap().status, 404);
}

#[test_case("echo 'gh: To use GitHub CLI in automation, set the GH_TOKEN environment variable.' >&2; exit 4", "gh: To use GitHub CLI in automation, set the GH_TOKEN environment variable." ; "no request")]
#[test_case("exit 3", "gh exit status: 3" ; "silent failure")]
#[test_case(r"printf 'HTTP/2.0 200 OK\r\n\r\n{}'; echo 'gh: broken pipe' >&2; exit 1", "gh: broken pipe" ; "failure after success")]
fn real_process_gh_transport_reports_gh_failures(script: &str, want: &str) {
    let (_dir, mut gh) = stub_gh(script);
    assert_eq!(gh.send(post()).unwrap_err().to_string(), want);
}

#[test]
fn real_process_gh_transport_reports_missing_gh() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = GhTransport::new(dir.path().join("gh"));
    let error = gh.send(post()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(error.to_string().starts_with("run gh: "), "{error}");
}

fn offline() -> io::Result<Response> {
    Err(io::Error::other("offline"))
}

fn denied() -> io::Result<Response> {
    respond(403, "denied\n")
}

fn not_json() -> io::Result<Response> {
    respond(200, "not-json")
}

#[test_case(offline, "GET /test: offline" ; "transport")]
#[test_case(denied, "GET /test: HTTP 403: denied" ; "http")]
#[test_case(not_json, "decode GET /test response: expected ident at line 1 column 2" ; "decode")]
fn request_errors_name_the_request(response: fn() -> io::Result<Response>, want: &str) {
    let (mut api, _) = fake_api(move |_| response());
    let error = api
        .decoded::<HashMap<String, String>>(Method::Get, "/test", None)
        .unwrap_err();
    assert_eq!(chain(&error), want);
}

#[test]
fn request_allows_named_statuses() {
    let (mut api, _) = fake_api(|_| respond(404, "{}"));
    assert_eq!(
        api.request(Method::Get, "/test", None, &[404]).unwrap(),
        (404, b"{}".to_vec())
    );
}

#[test_case("" ; "empty")]
#[test_case("owner" ; "no name")]
#[test_case("o/r/extra" ; "extra part")]
#[test_case("./r" ; "dot owner")]
#[test_case("o/.." ; "dot dot name")]
#[test_case("o /r" ; "space")]
#[test_case("o/" ; "empty name")]
fn refuses_invalid_repository(input: &str) {
    assert_eq!(
        chain(&Repository::parse(input).unwrap_err()),
        format!("invalid repository {input:?} (expected owner/name)")
    );
}

#[test]
fn parses_repository() {
    assert_eq!(
        Repository::parse("owner-name/repo.name").unwrap(),
        Repository {
            owner: "owner-name".to_owned(),
            name: "repo.name".to_owned(),
        }
    );
}

#[test_case(&[], "GITHUB_API_URL is unset" ; "nothing set")]
#[test_case(&[("GITHUB_API_URL", API), ("GITHUB_REPOSITORY", "o/r"), ("GH_TOKEN", "")], "GH_TOKEN is unset" ; "empty token")]
#[test_case(&[("GITHUB_API_URL", API), ("GITHUB_REPOSITORY", "o/r"), ("GITHUB_TOKEN", "t")], "GH_TOKEN is unset" ; "only the workflow token name")]
#[test_case(&[("GITHUB_API_URL", API), ("GITHUB_REPOSITORY", "o"), ("GH_TOKEN", "t")], "invalid repository \"o\" (expected owner/name)" ; "invalid repository")]
fn from_env_refuses(env: &[(&'static str, &str)], want: &str) {
    let env: HashMap<&str, String> = env.iter().map(|(k, v)| (*k, (*v).to_owned())).collect();
    let result = from_env(|key| env.get(key).cloned(), Box::new(NoTransport));
    assert_eq!(chain(&result.err().unwrap()), want);
}

struct NoTransport;

impl Transport for NoTransport {
    fn send(&mut self, request: Request) -> io::Result<Response> {
        panic!("unexpected request {request:?}")
    }
}

#[test]
fn escapes_all_but_unreserved_characters() {
    assert_eq!(escape("v1.0~a_b-c d/é"), "v1.0~a_b-c%20d%2F%C3%A9");
    assert_eq!(
        query(&[
            ("state", "all"),
            ("labels", "needs hardware"),
            ("page", "1")
        ]),
        "labels=needs%20hardware&page=1&state=all"
    );
}
