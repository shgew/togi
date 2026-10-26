use std::{io, path::Path};

use serde_json::{Value, json};
use test_case::test_case;
use togi_journal::ConfigSnapshot;
use togi_machine::Backend;

use crate::*;

fn parse(text: &str) -> Config {
    Config::parse(text).unwrap()
}

fn rejection(text: &str) -> String {
    togi_machine::error_chain(&Config::parse(text).unwrap_err())
}

fn customised() -> Config {
    parse(
        r#"{
            "dead_ends": {"inconclusive_in_a_row": 1, "stray_crashes_in_a_row": 100},
            "backends": {"mprime": "/opt/mprime", "ycruncher": "/opt/y-cruncher"},
            "backend_user": "togi-trial"
        }"#,
    )
}

#[test]
fn empty_object_is_the_defaults() {
    let config = parse("{}");
    assert_eq!(config, Config::default());
    assert_eq!(config.dead_ends().inconclusive_in_a_row().get(), 3);
    assert_eq!(config.dead_ends().stray_crashes_in_a_row().get(), 3);
    assert_eq!(config.backends().mprime(), None);
    assert_eq!(config.backends().ycruncher(), None);
    assert_eq!(config.backend_user(), None);
}

#[test]
fn null_sections_keep_defaults() {
    let text = r#"{"dead_ends":null,"backends":null,"backend_user":null}"#;
    assert_eq!(parse(text), Config::default());
}

#[test]
fn backends_and_user() {
    let config = parse(
        r#"{"backends":{"mprime":"/opt/mprime","ycruncher":null},"backend_user":"togi-trial"}"#,
    );
    assert_eq!(config.backends().mprime(), Some(Path::new("/opt/mprime")));
    assert_eq!(config.backends().ycruncher(), None);
    assert_eq!(
        config.backends().path(Backend::Mprime),
        Some(Path::new("/opt/mprime"))
    );
    assert_eq!(config.backends().path(Backend::Ycruncher), None);
    assert_eq!(config.backend_user(), Some("togi-trial"));
}

#[test_case(r#"{"start_offsets":{"3":-10}}"#, "start_offsets" ; "start offsets")]
#[test_case(r#"{"start_offsets":{}}"#, "start_offsets" ; "empty start offsets")]
#[test_case(r#"{"candidate_solo_limits":{"2":-36}}"#, "candidate_solo_limits" ; "candidate solo limits")]
#[test_case(r#"{"durations":{"search_trial_s":60}}"#, "durations.search_trial_s" ; "search trial")]
#[test_case(r#"{"durations":{"short_trial_s":60}}"#, "durations.short_trial_s" ; "short trial")]
#[test_case(r#"{"durations":{"checking_trial_s":60}}"#, "durations.checking_trial_s" ; "checking trial")]
#[test_case(r#"{"durations":{"checking_idle_s":60}}"#, "durations.checking_idle_s" ; "checking idle")]
#[test_case(r#"{"durations":{"checking_all_core_s":60}}"#, "durations.checking_all_core_s" ; "checking all core")]
#[test_case(r#"{"evidence":{"miss":0.1}}"#, "evidence.miss" ; "evidence miss")]
#[test_case(r#"{"evidence":{"rate":0.1}}"#, "evidence.rate" ; "evidence rate")]
#[test_case(r#"{"checking":{"cycle":["R1"]}}"#, "checking.cycle" ; "checking cycle")]
fn a_retired_key_is_refused_by_name(text: &str, key: &str) {
    let message = rejection(text);
    assert_eq!(
        message,
        format!("{key}: Ruleset 13 retired this setting; remove it from the configuration")
    );
    assert!(serde_json::from_str::<Config>(text).is_err());
}

#[test]
fn a_retired_key_is_refused_even_beside_valid_settings() {
    let message = rejection(r#"{"backend_user":"togi-trial","evidence":{"miss":0.1}}"#);
    assert!(
        message.starts_with("evidence.miss: Ruleset 13 retired"),
        "{message}"
    );
}

#[test_case(r#"{"durations":null,"evidence":null,"checking":null,"start_offsets":null,"candidate_solo_limits":null}"# ; "null sections")]
#[test_case(r#"{"durations":{},"evidence":{},"checking":{}}"# ; "empty sections")]
fn a_retired_section_that_sets_nothing_is_accepted(text: &str) {
    assert_eq!(parse(text), Config::default());
}

#[test_case(r#"{"backend_user":1001}"# ; "backend user number")]
#[test_case(r#"{"backends":{"mprime":7}}"# ; "backend number")]
#[test_case(r#"{"dead_ends":[]}"# ; "dead ends array")]
#[test_case(r#"{"dead_ends":{"inconclusive_in_a_row":"3"}}"# ; "string threshold")]
#[test_case(r#"{"dead_ends":{"inconclusive_in_a_row":3.5}}"# ; "fractional threshold")]
fn wrong_types(text: &str) {
    assert!(matches!(
        Config::parse(text).unwrap_err(),
        ConfigError::Syntax(_)
    ));
}

#[test_case("" ; "empty file")]
#[test_case(" \n\tnull\n " ; "null root")]
#[test_case("[]" ; "array root")]
#[test_case("3" ; "number root")]
#[test_case(r#"{"dead_ends":"# ; "truncated")]
#[test_case("{} {}" ; "trailing object")]
#[test_case("{} invalid" ; "trailing garbage")]
#[test_case(r#"{"backend_user":"a",}"# ; "trailing comma")]
#[test_case(r#"{"backend_user":"a"} // note"# ; "comment")]
fn malformed_documents(text: &str) {
    assert!(matches!(
        Config::parse(text).unwrap_err(),
        ConfigError::Syntax(_)
    ));
}

#[test_case("[null,null,null,null,null,null,null,null]" ; "root")]
#[test_case(r#"{"dead_ends":[5,10]}"# ; "dead ends")]
#[test_case(r#"{"backends":["/opt/mprime",null]}"# ; "backends")]
fn positional_arrays_rejected(text: &str) {
    assert!(matches!(
        Config::parse(text).unwrap_err(),
        ConfigError::Syntax(_)
    ));
    assert!(serde_json::from_str::<Config>(text).is_err());
    let value: Value = serde_json::from_str(text).unwrap();
    assert!(serde_json::from_value::<Config>(value).is_err());
}

#[test_case(r#"{"bogus":1}"# ; "top level")]
#[test_case(r#"{"durations":{"search_s":1}}"# ; "durations")]
#[test_case(r#"{"evidence":{"bogus":1}}"# ; "evidence")]
#[test_case(r#"{"checking":{"bogus":1}}"# ; "checking")]
#[test_case(r#"{"dead_ends":{"bogus":1}}"# ; "dead ends")]
#[test_case(r#"{"backends":{"bogus":"/bin/backend"}}"# ; "backends")]
fn unknown_members(text: &str) {
    let message = rejection(text);
    assert!(message.contains("unknown field"), "{message}");
}

#[test_case(r#"{"backend_user":"first","backend_user":"second"}"# ; "top level")]
#[test_case(r#"{"dead_ends":{"inconclusive_in_a_row":1,"inconclusive_in_a_row":2}}"# ; "nested")]
fn duplicate_members(text: &str) {
    let message = rejection(text);
    assert!(message.contains("duplicate field"), "{message}");
}

#[test_case(r#"{"dead_ends":{"inconclusive_in_a_row":1}}"#, 1, 3 ; "lower")]
#[test_case(r#"{"dead_ends":{"inconclusive_in_a_row":100}}"#, 100, 3 ; "upper")]
#[test_case(r#"{"dead_ends":{"stray_crashes_in_a_row":1}}"#, 3, 1 ; "stray lower")]
#[test_case(r#"{"dead_ends":{"stray_crashes_in_a_row":100}}"#, 3, 100 ; "stray upper")]
fn thresholds_accepted(text: &str, inconclusive: u32, stray: u32) {
    let dead_ends = parse(text).dead_ends();
    assert_eq!(dead_ends.inconclusive_in_a_row().get(), inconclusive);
    assert_eq!(dead_ends.stray_crashes_in_a_row().get(), stray);
}

#[test_case("inconclusive_in_a_row", 0 ; "inconclusive zero")]
#[test_case("inconclusive_in_a_row", 101 ; "inconclusive above")]
#[test_case("stray_crashes_in_a_row", 0 ; "stray zero")]
#[test_case("stray_crashes_in_a_row", 101 ; "stray above")]
#[test_case("stray_crashes_in_a_row", -1 ; "stray negative")]
fn thresholds_rejected(key: &str, value: i64) {
    let message = rejection(&json!({"dead_ends": {key: value}}).to_string());
    assert!(
        message.starts_with(&format!("dead_ends.{key}: {value} is not within [1, 100]")),
        "{message}"
    );
}

#[test_case(r#"{"backends":{"mprime":"bin/mprime"}}"#, "mprime" ; "relative mprime")]
#[test_case(r#"{"backends":{"ycruncher":"./y"}}"#, "ycruncher" ; "relative ycruncher")]
#[test_case(r#"{"backends":{"mprime":""}}"#, "mprime" ; "empty mprime")]
#[test_case(r#"{"backends":{"ycruncher":"~/y"}}"#, "ycruncher" ; "tilde")]
fn relative_backend_paths_rejected(text: &str, backend: &str) {
    let err = Config::parse(text).unwrap_err();
    assert!(matches!(err, ConfigError::Backend { .. }), "{err}");
    assert!(
        togi_machine::error_chain(&err).starts_with(&format!("backends.{backend}:")),
        "{err}"
    );
}

#[test]
fn empty_backend_user_rejected() {
    assert!(matches!(
        Config::parse(r#"{"backend_user":""}"#).unwrap_err(),
        ConfigError::EmptyBackendUser
    ));
}

#[test]
fn constructors_enforce_their_invariants() {
    assert!(DeadEndThreshold::new(101).is_err());
    assert!(Backends::new(Some("relative".into()), None).is_err());
    assert!(Backends::new(Some("/abs".into()), Some("/abs/y".into())).is_ok());
}

#[test]
fn the_default_snapshot_writes_every_current_setting_and_no_retired_one() {
    let value = serde_json::to_value(Config::default().snapshot()).unwrap();
    assert_eq!(
        value,
        json!({
            "dead_ends": {"inconclusive_in_a_row": 3, "stray_crashes_in_a_row": 3},
            "backends": {"mprime": null, "ycruncher": null},
            "backend_user": null
        })
    );
}

#[test]
fn a_customised_snapshot_carries_every_effective_value() {
    let value = serde_json::to_value(customised().snapshot()).unwrap();
    assert_eq!(
        value,
        json!({
            "dead_ends": {"inconclusive_in_a_row": 1, "stray_crashes_in_a_row": 100},
            "backends": {"mprime": "/opt/mprime", "ycruncher": "/opt/y-cruncher"},
            "backend_user": "togi-trial"
        })
    );
}

#[test_case(&Config::default() ; "default")]
#[test_case(&customised() ; "customised")]
fn a_snapshot_reads_back_to_the_same_configuration(config: &Config) {
    let snapshot = config.snapshot();
    assert_eq!(&Config::from_snapshot(&snapshot).unwrap(), config);
    let wire = serde_json::to_string(&snapshot).unwrap();
    let decoded = serde_json::from_str::<ConfigSnapshot>(&wire).unwrap();
    assert_eq!(decoded, snapshot);
    assert_eq!(&Config::from_snapshot(&decoded).unwrap(), config);
}

#[test]
fn a_historical_snapshot_with_retired_settings_still_reads_back_without_them() {
    let historical = json!({
        "start_offsets": {"3": -10},
        "candidate_solo_limits": {},
        "durations": {
            "search_trial_s": 60, "short_trial_s": 120, "checking_trial_s": 1,
            "checking_idle_s": 900, "checking_all_core_s": 4
        },
        "evidence": {"miss": 0.05, "rate": 0.5},
        "checking": {"cycle": ["R1"]},
        "dead_ends": {"inconclusive_in_a_row": 3, "stray_crashes_in_a_row": 3},
        "backends": {"mprime": null, "ycruncher": null},
        "backend_user": null
    });
    let snapshot = serde_json::from_value::<ConfigSnapshot>(historical).unwrap();
    assert_eq!(Config::from_snapshot(&snapshot).unwrap(), Config::default());
}

#[test_case(|snapshot| snapshot.dead_ends.stray_crashes_in_a_row = 101, "stray_crashes_in_a_row" ; "stray threshold above range")]
#[test_case(|snapshot| snapshot.backends.mprime = Some("relative/path".into()), "mprime" ; "relative backend path")]
#[test_case(|snapshot| snapshot.backend_user = Some(String::new()), "backend_user" ; "empty backend user")]
fn a_snapshot_the_configuration_would_refuse_is_refused_when_read_back(
    corrupt: fn(&mut ConfigSnapshot),
    needle: &str,
) {
    let mut snapshot = Config::default().snapshot();
    corrupt(&mut snapshot);
    let message = togi_machine::error_chain(&Config::from_snapshot(&snapshot).unwrap_err());
    assert!(message.contains(needle), "{needle}: {message}");
}

#[test]
fn serde_text_preserves_the_rejected_value_and_range() {
    let error =
        serde_json::from_str::<Config>(r#"{"dead_ends":{"inconclusive_in_a_row":0}}"#).unwrap_err();
    assert_eq!(
        error.to_string(),
        "dead_ends.inconclusive_in_a_row: 0 is not within [1, 100]"
    );
}

#[test]
fn load_reads_content() {
    let path = std::path::Path::new("/config/config.json");
    assert_eq!(
        Config::load_with(path, |requested| {
            assert_eq!(requested, path);
            Ok(br#"{"backend_user":"togi-trial"}"#.as_slice())
        })
        .unwrap()
        .backend_user(),
        Some("togi-trial")
    );
}

#[test]
fn load_of_a_missing_file_stays_inspectable() {
    let missing = std::path::Path::new("/config/missing.json");
    let error = Config::load_with(missing, |requested| {
        assert_eq!(requested, missing);
        Err::<&[u8], _>(io::Error::new(
            io::ErrorKind::NotFound,
            "missing configuration",
        ))
    })
    .unwrap_err();
    assert!(error.to_string().contains(missing.to_str().unwrap()));
    match error {
        LoadError::Read { path, source } => {
            assert_eq!(path, missing);
            assert_eq!(source.kind(), io::ErrorKind::NotFound);
            assert_eq!(source.to_string(), "missing configuration");
        }
        other @ LoadError::Parse { .. } => panic!("{other}"),
    }
}

#[test]
fn load_names_the_file_of_an_invalid_config() {
    let path = std::path::Path::new("/config/config.json");
    let err = Config::load_with(path, |_| Ok(br#"{"bogus":1}"#.as_slice())).unwrap_err();
    assert!(matches!(&err, LoadError::Parse { path: named, .. } if named == path));
    let text = togi_machine::error_chain(&err);
    assert!(text.contains("unknown field"), "{text}");
    assert!(text.contains(path.to_str().unwrap()), "{text}");
}

#[test]
fn a_retired_key_in_a_loaded_file_names_the_file_then_the_key() {
    let path = std::path::Path::new("/config/config.json");
    let err = Config::load_with(path, |_| Ok(br#"{"checking":{"cycle":["R1"]}}"#.as_slice()))
        .unwrap_err();
    assert_eq!(
        togi_machine::error_chain(&err),
        "invalid config /config/config.json: checking.cycle: Ruleset 13 retired this setting; remove it from the configuration"
    );
}

#[test]
fn a_rejected_value_renders_file_then_field_then_range_and_keeps_each_source() {
    let path = std::path::Path::new("/config/config.json");
    let err = Config::load_with(path, |_| {
        Ok(r#"{"dead_ends":{"stray_crashes_in_a_row":0}}"#.as_bytes())
    })
    .unwrap_err();
    assert_eq!(err.to_string(), "invalid config /config/config.json");
    assert_eq!(
        togi_machine::error_chain(&err),
        "invalid config /config/config.json: dead_ends.stray_crashes_in_a_row: 0 is not within [1, 100]"
    );
    let config = std::error::Error::source(&err)
        .and_then(|source| source.downcast_ref::<ConfigError>())
        .expect("the load error keeps the config error as its source");
    assert_eq!(config.to_string(), "dead_ends.stray_crashes_in_a_row");
    let range = std::error::Error::source(config)
        .and_then(|source| source.downcast_ref::<RangeError>())
        .expect("the config error keeps the range error as its source");
    assert_eq!(range.to_string(), "0 is not within [1, 100]");
}

#[test]
fn a_syntax_error_in_a_loaded_file_keeps_every_level_as_a_downcastable_source() {
    let path = std::path::Path::new("/config/config.json");
    let err = Config::load_with(path, |_| Ok(b"{".as_slice())).unwrap_err();
    assert_eq!(err.to_string(), "invalid config /config/config.json");
    let config = std::error::Error::source(&err)
        .and_then(|source| source.downcast_ref::<ConfigError>())
        .expect("the load error keeps the config error as its source");
    assert!(matches!(config, ConfigError::Syntax(_)));
    assert_eq!(config.to_string(), "parse config JSON");
    let syntax = std::error::Error::source(config)
        .and_then(|source| source.downcast_ref::<serde_json::Error>())
        .expect("the config error keeps the serde error as its source");
    assert_eq!(
        togi_machine::error_chain(&err),
        format!("invalid config /config/config.json: parse config JSON: {syntax}")
    );
}

#[test]
fn load_rejects_a_file_that_is_not_utf8() {
    let path = std::path::Path::new("/config/config.json");
    assert!(matches!(
        Config::load_with(path, |_| Ok([0xff, 0xfe].as_slice())).unwrap_err(),
        LoadError::Read { path: named, source }
            if named == path && source.kind() == io::ErrorKind::InvalidData
    ));
}

#[test]
fn default_path_is_the_etc_file() {
    assert_eq!(DEFAULT_PATH, "/etc/togi/config.json");
}
