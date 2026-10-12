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

#[test]
fn anything_that_is_not_a_session_start_with_a_session_and_a_build_is_not_a_header() {
    for (name, line) in [
        ("empty", String::new()),
        ("text", "garbage".to_owned()),
        ("array", "[]".to_owned()),
        (
            "other kind",
            START.replace("session.start", "session.warning"),
        ),
        ("no kind", START.replace("\"kind\":\"session.start\",", "")),
        ("no data", r#"{"seq":1,"kind":"session.start"}"#.to_owned()),
        (
            "no build",
            r#"{"seq":1,"kind":"session.start","data":{"session":"20231114T221320Z"}}"#.to_owned(),
        ),
        (
            "no session",
            START.replace("\"session\":\"20231114T221320Z\",", ""),
        ),
        (
            "a session no Rust header could carry",
            START.replace("20231114T221320Z", "not-a-session"),
        ),
        (
            "a session that is not a string",
            START.replace("\"20231114T221320Z\"", "7"),
        ),
        (
            "build is not an object",
            START.replace(
                "\"build\":{\"version\":\"1\",\"ruleset\":3,\"schema\":1,\"evidence\":2}",
                "\"build\":[\"1\",3,1,2]",
            ),
        ),
        (
            "stamp without a ruleset",
            START.replace("\"ruleset\":3,", ""),
        ),
        (
            "stamp field of another type",
            START.replace("\"schema\":1", "\"schema\":\"1\""),
        ),
    ] {
        assert!(classify(line.as_bytes()).is_none(), "{name}");
    }
}
