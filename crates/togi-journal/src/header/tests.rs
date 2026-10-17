use test_case::test_case;

use super::classify;

const START: &str = r#"{"seq":1,"kind":"session.start","data":{"build":{"version":"1","ruleset":3,"schema":1,"evidence":2},"session":"20231114T221320Z","cores":[]}}"#;

#[test]
fn a_header_gives_its_build_stamp() {
    let build = classify(START.as_bytes()).unwrap();
    assert_eq!(
        (
            build.version.as_str(),
            build.ruleset,
            build.schema,
            build.fixes,
            build.evidence
        ),
        ("1", 3, 1, 0, 2)
    );
}

#[test]
fn additions_a_newer_build_made_do_not_hide_the_stamp() {
    let newer = START
        .replace("\"evidence\":2", "\"evidence\":2,\"channel\":\"x\"")
        .replace("\"cores\":[]", "\"cores\":[],\"later\":{}")
        .replace("\"seq\":1", "\"seq\":1,\"extra\":true");
    assert_ne!(newer, START);
    let build = classify(newer.as_bytes()).unwrap();
    assert_eq!(build.evidence, 2);
}

#[test_case(""; "empty")]
#[test_case("garbage"; "text")]
#[test_case("[]"; "array")]
#[test_case(&START.replace("session.start", "session.warning"); "other kind")]
#[test_case(&START.replace("\"kind\":\"session.start\",", ""); "no kind")]
#[test_case(r#"{"seq":1,"kind":"session.start"}"#; "no data")]
#[test_case(
    r#"{"seq":1,"kind":"session.start","data":{"session":"20231114T221320Z"}}"#;
    "no build"
)]
#[test_case(&START.replace("\"session\":\"20231114T221320Z\",", ""); "no session")]
#[test_case(
    &START.replace("20231114T221320Z", "not-a-session");
    "a session no journal header could carry"
)]
#[test_case(&START.replace("\"20231114T221320Z\"", "7"); "a session that is not a string")]
#[test_case(
    &START.replace(
        "\"build\":{\"version\":\"1\",\"ruleset\":3,\"schema\":1,\"evidence\":2}",
        "\"build\":[\"1\",3,1,2]",
    );
    "build is not an object"
)]
#[test_case(&START.replace("\"ruleset\":3,", ""); "stamp without a ruleset")]
#[test_case(&START.replace("\"schema\":1", "\"schema\":\"1\""); "stamp field of another type")]
fn anything_that_is_not_a_session_start_with_a_session_and_a_build_is_not_a_header(line: &str) {
    assert!(classify(line.as_bytes()).is_none());
}
