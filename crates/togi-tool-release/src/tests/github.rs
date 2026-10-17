use std::collections::HashMap;
use std::io::{self, Read};

use test_case::test_case;

use super::*;
use crate::chain;
use crate::fakes::{API, fake_api, respond};

struct BrokenBody;

impl Read for BrokenBody {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("broken response"))
    }
}

#[test]
fn request_sends_token_version_and_body() {
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
                ("Authorization", "Bearer test-token".to_owned()),
                ("Accept", "application/vnd.github+json".to_owned()),
                ("X-GitHub-Api-Version", "2022-11-28".to_owned()),
                ("User-Agent", "togi-tool-release".to_owned()),
                ("Content-Type", "application/json".to_owned()),
            ],
            body: Some(r#"{"name":"0.1.0"}"#.to_owned()),
        }]
    );
}

fn offline() -> io::Result<Response> {
    Err(io::Error::other("offline"))
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "handlers return the transport's result"
)]
fn broken_body() -> io::Result<Response> {
    Ok(Response {
        status: 200,
        body: Box::new(BrokenBody),
    })
}

fn denied() -> io::Result<Response> {
    respond(403, "denied\n")
}

fn not_json() -> io::Result<Response> {
    respond(200, "not-json")
}

#[test_case(offline, "GET /test: offline" ; "transport")]
#[test_case(broken_body, "read GET /test response: broken response" ; "read")]
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
#[test_case(&[("GITHUB_API_URL", API), ("GITHUB_REPOSITORY", "o/r"), ("GITHUB_TOKEN", "")], "GITHUB_TOKEN is unset" ; "empty token")]
#[test_case(&[("GITHUB_API_URL", API), ("GITHUB_REPOSITORY", "o"), ("GITHUB_TOKEN", "t")], "invalid repository \"o\" (expected owner/name)" ; "invalid repository")]
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
