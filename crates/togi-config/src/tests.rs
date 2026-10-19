use std::{io, path::Path, time::Duration};

use serde_json::{Value, json};
use test_case::test_case;
use togi_journal::ConfigSnapshot;
use togi_machine::{Backend, CoreId, Regime, TargetOffset};

use crate::*;

fn core(id: i64) -> CoreId {
    CoreId::new(id).unwrap()
}

fn offset(value: i64) -> TargetOffset {
    TargetOffset::new(value).unwrap()
}

fn parse(text: &str) -> Config {
    Config::parse(text).unwrap()
}

fn rejection(text: &str) -> String {
    togi_machine::error_chain(&Config::parse(text).unwrap_err())
}

fn trials(miss: f64, rate: f64) -> Result<u32, TrialsError> {
    EvidencePolicy::new(
        Probability::new(miss).unwrap(),
        Probability::new(rate).unwrap(),
    )
    .map(|policy| policy.trials().get())
}

fn customised() -> Config {
    parse(
        r#"{
            "start_offsets": {"3": -10, "15": 0},
            "candidate_solo_limits": {"2": -36},
            "durations": {
                "search_trial_s": 60, "short_trial_s": 86400, "checking_trial_s": 1,
                "checking_idle_s": 7200, "checking_all_core_s": 4
            },
            "evidence": {"miss": 0.001, "rate": 0.25},
            "checking": {"cycle": ["R6", "R1", "R6"]},
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
    assert_eq!(config.search_trial().get(), 90);
    assert_eq!(config.short_trial().get(), 120);
    assert_eq!(config.checking_trial().get(), 120);
    assert_eq!(config.checking_idle().get(), 900);
    assert_eq!(config.checking_all_core().get(), 1200);
    assert_eq!(config.evidence().miss().get(), 0.05);
    assert_eq!(config.evidence().rate().get(), 0.5);
    assert_eq!(config.dead_ends().inconclusive_in_a_row().get(), 3);
    assert_eq!(config.dead_ends().stray_crashes_in_a_row().get(), 3);
    assert_eq!(config.backends().mprime(), None);
    assert_eq!(config.backends().ycruncher(), None);
    assert_eq!(config.backend_user(), None);
    assert_eq!(config.overrides().count(), 0);
}

#[test]
fn default_evidence_count_matches_the_formula() {
    let policy = Config::default().evidence();
    assert_eq!(Ok(policy.trials().get()), trials(0.05, 0.5));
}

#[test]
fn default_cycle() {
    use Regime::{R1, R2, R3, R4, R5, R6, R7};
    assert_eq!(
        Config::default().checking_cycle().as_slice(),
        [
            R7, R7, R7, R2, R2, R2, R6, R5, R1, R1, R1, R3, R3, R3, R4, R4, R4, R6
        ]
    );
}

#[test]
fn partial_files_keep_other_defaults() {
    let config = parse(r#"{"durations":{"search_trial_s":60},"evidence":{"rate":0.25}}"#);
    assert_eq!(config.search_trial().get(), 60);
    assert_eq!(config.short_trial().get(), 120);
    assert_eq!(config.evidence().miss().get(), 0.05);
    assert_eq!(config.evidence().rate().get(), 0.25);
    assert_eq!(config.evidence().trials().get(), 11);
}

#[test]
fn null_sections_keep_defaults() {
    let text = r#"{"durations":null,"evidence":null,"checking":null,"dead_ends":null,
        "backends":null,"backend_user":null,"start_offsets":null,"candidate_solo_limits":null}"#;
    assert_eq!(parse(text), Config::default());
}

#[test]
fn empty_override_tables_are_none() {
    assert_eq!(
        parse(r#"{"start_offsets":{},"candidate_solo_limits":{}}"#),
        Config::default()
    );
}

#[test]
fn overrides_by_core() {
    let config =
        parse(r#"{"start_offsets":{"11":-5,"3":0},"candidate_solo_limits":{"2":-36,"15":-50}}"#);
    assert_eq!(
        config.override_for(core(3)),
        Some(InitialOverride::Start(offset(0)))
    );
    assert_eq!(
        config.override_for(core(2)),
        Some(InitialOverride::CandidateSoloLimit(offset(-36)))
    );
    assert_eq!(config.override_for(core(4)), None);
    assert_eq!(
        config.overrides().collect::<Vec<_>>(),
        [
            (core(2), InitialOverride::CandidateSoloLimit(offset(-36))),
            (core(3), InitialOverride::Start(offset(0))),
            (core(11), InitialOverride::Start(offset(-5))),
            (core(15), InitialOverride::CandidateSoloLimit(offset(-50))),
        ]
    );
    assert_eq!(config.override_for(core(15)).unwrap().offset(), offset(-50));
}

#[test]
fn candidate_solo_limit_beside_another_cores_start_offset() {
    let config = parse(r#"{"start_offsets":{"2":-5},"candidate_solo_limits":{"3":-36}}"#);
    assert_eq!(config.overrides().count(), 2);
}

#[test]
fn cycle_replaced_with_repeats_in_any_order() {
    let config = parse(r#"{"checking":{"cycle":["R7"]}}"#);
    assert_eq!(config.checking_cycle().as_slice(), [Regime::R7]);
    let config = parse(r#"{"checking":{"cycle":["R6","R1","R6","R6"]}}"#);
    assert_eq!(
        (config.checking_cycle()).into_iter().collect::<Vec<_>>(),
        [Regime::R6, Regime::R1, Regime::R6, Regime::R6]
    );
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

#[test_case("search_trial_s", 1 ; "search lower")]
#[test_case("search_trial_s", 86400 ; "search upper")]
#[test_case("short_trial_s", 1 ; "short lower")]
#[test_case("short_trial_s", 86400 ; "short upper")]
#[test_case("checking_trial_s", 1 ; "checking lower")]
#[test_case("checking_trial_s", 86400 ; "checking upper")]
#[test_case("checking_idle_s", 1 ; "idle lower")]
#[test_case("checking_idle_s", 86400 ; "idle upper")]
#[test_case("checking_all_core_s", 4 ; "all-core lower")]
#[test_case("checking_all_core_s", 86400 ; "all-core upper")]
fn duration_bounds_accepted(key: &str, seconds: i64) {
    let config = parse(&json!({"durations": {key: seconds}}).to_string());
    let stored = match key {
        "search_trial_s" => config.search_trial().get(),
        "short_trial_s" => config.short_trial().get(),
        "checking_trial_s" => config.checking_trial().get(),
        "checking_idle_s" => config.checking_idle().get(),
        _ => config.checking_all_core().get(),
    };
    assert_eq!(i64::from(stored), seconds);
}

#[test_case("search_trial_s", 0 ; "search zero")]
#[test_case("search_trial_s", 86401 ; "search above a day")]
#[test_case("short_trial_s", 0 ; "short zero")]
#[test_case("short_trial_s", 86401 ; "short above a day")]
#[test_case("checking_trial_s", 0 ; "checking zero")]
#[test_case("checking_trial_s", 86401 ; "checking above a day")]
#[test_case("checking_idle_s", 0 ; "idle zero")]
#[test_case("checking_idle_s", 86401 ; "idle above a day")]
#[test_case("checking_all_core_s", 3 ; "all-core below four")]
#[test_case("checking_all_core_s", 0 ; "all-core zero")]
#[test_case("checking_all_core_s", 86401 ; "all-core above a day")]
#[test_case("search_trial_s", -90 ; "negative")]
fn duration_bounds_rejected(key: &str, seconds: i64) {
    let message = rejection(&json!({"durations": {key: seconds}}).to_string());
    assert!(
        message.starts_with(&format!("durations.{key}: {seconds} is not within [")),
        "{message}"
    );
}

#[test]
fn durations_are_whole_second_durations() {
    let config = Config::default();
    assert_eq!(config.short_trial().as_duration(), Duration::from_secs(120));
    assert_eq!(
        config.checking_all_core().as_duration(),
        Duration::from_mins(20)
    );
}

#[test_case(r#"{"durations":{"search_trial_s":"90"}}"# ; "string duration")]
#[test_case(r#"{"durations":{"search_trial_s":90.5}}"# ; "fractional duration")]
#[test_case(r#"{"durations":{"search_trial_s":1e2}}"# ; "exponent duration")]
#[test_case(r#"{"durations":{"search_trial_s":99999999999999999999}}"# ; "duration beyond i64")]
#[test_case(r#"{"backend_user":1001}"# ; "backend user number")]
#[test_case(r#"{"checking":{"cycle":"R7"}}"# ; "cycle string")]
#[test_case(r#"{"evidence":{"miss":"0.05"}}"# ; "miss string")]
#[test_case(r#"{"start_offsets":{"3":"-10"}}"# ; "offset string")]
#[test_case(r#"{"start_offsets":{"3":-10.0}}"# ; "offset float")]
#[test_case(r#"{"start_offsets":[]}"# ; "offsets array")]
#[test_case(r#"{"backends":{"mprime":7}}"# ; "backend number")]
#[test_case(r#"{"dead_ends":[]}"# ; "dead ends array")]
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
#[test_case(r#"{"durations":"# ; "truncated")]
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
#[test_case(r#"{"durations":[60,null,null,null,null]}"# ; "durations")]
#[test_case(r#"{"evidence":[0.001,0.25]}"# ; "evidence")]
#[test_case(r#"{"checking":[["R7"]]}"# ; "checking")]
#[test_case(r#"{"dead_ends":[5,10]}"# ; "dead ends")]
#[test_case(r#"{"backends":["/opt/mprime",null]}"# ; "backends")]
#[test_case(r#"{"start_offsets":[-10]}"# ; "start offsets")]
#[test_case(r#"{"candidate_solo_limits":[-36]}"# ; "candidate solo limits")]
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
#[test_case(r#"{"durations":{"confirmation_trial_s":300}}"# ; "removed confirmation key")]
#[test_case(r#"{"durations":{"start_s":120}}"# ; "old duration key")]
#[test_case(r#"{"evidence":{"bogus":1}}"# ; "evidence")]
#[test_case(r#"{"evidence":{"failure_rate":0.1}}"# ; "removed failure rate")]
#[test_case(r#"{"evidence":{"significance":0.3}}"# ; "removed significance")]
#[test_case(r#"{"checking":{"bogus":1}}"# ; "checking")]
#[test_case(r#"{"checking":{"lap":["R7"]}}"# ; "old checking key")]
#[test_case(r#"{"dead_ends":{"bogus":1}}"# ; "dead ends")]
#[test_case(r#"{"backends":{"bogus":"/bin/backend"}}"# ; "backends")]
fn unknown_members(text: &str) {
    let message = rejection(text);
    assert!(message.contains("unknown field"), "{message}");
}

#[test_case(r#"{"backend_user":"first","backend_user":"second"}"# ; "top level")]
#[test_case(r#"{"durations":{"search_trial_s":60,"search_trial_s":90}}"# ; "nested")]
#[test_case(r#"{"evidence":{"miss":0.1,"miss":0.2}}"# ; "evidence")]
fn duplicate_members(text: &str) {
    let message = rejection(text);
    assert!(message.contains("duplicate field"), "{message}");
}

#[test_case(r#"{"evidence":{"miss":0}}"#, "evidence.miss: 0 is not a number strictly between 0 and 1" ; "zero miss")]
#[test_case(r#"{"evidence":{"miss":1}}"#, "evidence.miss: 1 is not a number strictly between 0 and 1" ; "one miss")]
#[test_case(r#"{"evidence":{"miss":-0.5}}"#, "evidence.miss" ; "negative miss")]
#[test_case(r#"{"evidence":{"miss":1.5}}"#, "evidence.miss" ; "miss above one")]
#[test_case(r#"{"evidence":{"rate":0}}"#, "evidence.rate: 0 is not a number strictly between 0 and 1" ; "zero rate")]
#[test_case(r#"{"evidence":{"rate":1}}"#, "evidence.rate: 1 is not a number strictly between 0 and 1" ; "one rate")]
#[test_case(r#"{"evidence":{"rate":1e-300}}"#, "do not give between 1 and 1000 trials" ; "rate too small")]
fn evidence_rejected(text: &str, message: &str) {
    let got = rejection(text);
    assert!(got.contains(message), "{got}");
}

#[test_case(0.0 ; "zero")]
#[test_case(1.0 ; "one")]
#[test_case(-0.0 ; "negative zero")]
#[test_case(-1.0 ; "negative one")]
#[test_case(2.0 ; "two")]
#[test_case(f64::NAN ; "nan")]
#[test_case(f64::INFINITY ; "infinity")]
#[test_case(f64::NEG_INFINITY ; "negative infinity")]
fn probability_constructor_rejects_endpoints_and_non_finite(value: f64) {
    assert!(Probability::new(value).is_err(), "{value}");
}

#[test]
fn probability_conversion_rejects_nan() {
    assert!(Probability::try_from(f64::NAN).is_err());
}

#[test_case(f64::MIN_POSITIVE ; "smallest normal")]
#[test_case(1e-300 ; "tiny")]
#[test_case(0.5 ; "half")]
#[test_case(1.0 - f64::EPSILON / 2.0 ; "just below one")]
fn probability_constructor_accepts_the_open_interval(value: f64) {
    assert_eq!(Probability::new(value).unwrap().get(), value);
    assert_eq!(Probability::try_from(value), Probability::new(value));
}

#[test_case(0.05, 0.5, 5 ; "default")]
#[test_case(0.001, 0.5, 10 ; "smaller miss")]
#[test_case(0.05, 0.25, 11 ; "smaller rate")]
fn evidence_trials(miss: f64, rate: f64, want: u32) {
    assert_eq!(trials(miss, rate), Ok(want));
}

#[test]
fn trial_count_bounds() {
    assert_eq!(trials(0.5_f64.powf(999.5), 0.5), Ok(1000));
    assert_eq!(
        trials(0.5_f64.powf(1000.5), 0.5),
        Err(TrialsError {
            miss: 0.5_f64.powf(1000.5),
            rate: 0.5
        })
    );
    assert_eq!(trials(1.0 - f64::EPSILON / 2.0, 0.5), Ok(1));
}

#[test]
fn extreme_probabilities_never_yield_a_zero_or_unbounded_count() {
    let extremes = [
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        1e-300,
        1e-17,
        0.5,
        1.0 - f64::EPSILON / 2.0,
    ];
    for miss in extremes {
        for rate in extremes {
            if let Ok(count) = trials(miss, rate) {
                assert!((1..=1000).contains(&count), "{miss} {rate}");
            }
        }
    }
    assert!(trials(f64::from_bits(1), f64::from_bits(1)).is_err());
    assert!(trials(0.05, f64::from_bits(1)).is_err());
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

#[test_case(r#"{"checking":{"cycle":[]}}"#, "checking.cycle" ; "empty cycle")]
#[test_case(r#"{"checking":{"cycle":["R1","R8"]}}"#, "R8" ; "not a regime")]
#[test_case(r#"{"checking":{"cycle":["r1"]}}"#, "r1" ; "lowercase regime")]
fn cycles_rejected(text: &str, message: &str) {
    let got = rejection(text);
    assert!(got.contains(message), "{got}");
}

const START: &str = "start_offsets";
const SOLO: &str = "candidate_solo_limits";

#[test_case(START, "3", -10 ; "start offsets single digit")]
#[test_case(START, "0", 0 ; "start offsets zero core")]
#[test_case(START, "10", -50 ; "start offsets two digits at the floor")]
#[test_case(START, "15", -1 ; "start offsets last core")]
#[test_case(SOLO, "3", -10 ; "solo limits single digit")]
#[test_case(SOLO, "0", 0 ; "solo limits zero core")]
#[test_case(SOLO, "10", -50 ; "solo limits two digits at the floor")]
#[test_case(SOLO, "15", -1 ; "solo limits last core")]
fn core_keys_accepted_in_both_tables(table: &str, key: &str, value: i64) {
    let config = parse(&json!({table: {key: value}}).to_string());
    let (id, over) = config.overrides().next().unwrap();
    assert_eq!(id.to_string(), key);
    assert_eq!(over.offset().get(), value);
}

#[test_case(START, "03" ; "start offsets leading zero")]
#[test_case(START, "00" ; "start offsets double zero")]
#[test_case(START, "+3" ; "start offsets plus sign")]
#[test_case(START, "-1" ; "start offsets negative")]
#[test_case(START, "-0" ; "start offsets negative zero")]
#[test_case(START, " 3" ; "start offsets leading space")]
#[test_case(START, "3 " ; "start offsets trailing space")]
#[test_case(START, "" ; "start offsets empty key")]
#[test_case(START, "x" ; "start offsets letters")]
#[test_case(START, "3.0" ; "start offsets decimal point")]
#[test_case(START, "1e1" ; "start offsets exponent")]
#[test_case(START, "0x3" ; "start offsets hex")]
#[test_case(START, "٣" ; "start offsets non-ascii digit")]
#[test_case(START, "99999999999999999999" ; "start offsets beyond i64")]
#[test_case(SOLO, "03" ; "solo limits leading zero")]
#[test_case(SOLO, "00" ; "solo limits double zero")]
#[test_case(SOLO, "+3" ; "solo limits plus sign")]
#[test_case(SOLO, "-1" ; "solo limits negative")]
#[test_case(SOLO, "-0" ; "solo limits negative zero")]
#[test_case(SOLO, " 3" ; "solo limits leading space")]
#[test_case(SOLO, "3 " ; "solo limits trailing space")]
#[test_case(SOLO, "" ; "solo limits empty key")]
#[test_case(SOLO, "x" ; "solo limits letters")]
#[test_case(SOLO, "3.0" ; "solo limits decimal point")]
#[test_case(SOLO, "1e1" ; "solo limits exponent")]
#[test_case(SOLO, "0x3" ; "solo limits hex")]
#[test_case(SOLO, "٣" ; "solo limits non-ascii digit")]
#[test_case(SOLO, "99999999999999999999" ; "solo limits beyond i64")]
fn non_canonical_core_keys_rejected(table: &str, key: &str) {
    let err = Config::parse(&json!({table: {key: -1}}).to_string()).unwrap_err();
    assert!(matches!(err, ConfigError::CoreKey { .. }), "{key:?}: {err}");
}

#[test_case(START, "16" ; "start offsets first beyond")]
#[test_case(START, "99" ; "start offsets two digits beyond")]
#[test_case(START, "255" ; "start offsets u8 edge")]
#[test_case(START, "256" ; "start offsets past u8")]
#[test_case(SOLO, "16" ; "solo limits first beyond")]
#[test_case(SOLO, "99" ; "solo limits two digits beyond")]
#[test_case(SOLO, "255" ; "solo limits u8 edge")]
#[test_case(SOLO, "256" ; "solo limits past u8")]
fn core_ids_beyond_fifteen_rejected(table: &str, key: &str) {
    let err = Config::parse(&json!({table: {key: -1}}).to_string()).unwrap_err();
    assert!(matches!(err, ConfigError::Core { .. }), "{key:?}: {err}");
}

#[test_case(START, 1 ; "start offsets positive")]
#[test_case(START, 50 ; "start offsets far positive")]
#[test_case(START, -51 ; "start offsets below floor")]
#[test_case(START, -1000 ; "start offsets far below floor")]
#[test_case(START, i64::MIN ; "start offsets minimum")]
#[test_case(SOLO, 1 ; "solo limits positive")]
#[test_case(SOLO, 50 ; "solo limits far positive")]
#[test_case(SOLO, -51 ; "solo limits below floor")]
#[test_case(SOLO, -1000 ; "solo limits far below floor")]
#[test_case(SOLO, i64::MIN ; "solo limits minimum")]
fn offsets_out_of_range_rejected(table: &str, value: i64) {
    let err = Config::parse(&json!({table: {"3": value}}).to_string()).unwrap_err();
    assert!(matches!(err, ConfigError::Offset { .. }), "{value}: {err}");
    assert!(err.to_string().contains(table), "{err}");
}

#[test]
fn offsets_inside_range_are_never_clamped() {
    let config = parse(r#"{"start_offsets":{"1":-50,"2":0,"3":-1}}"#);
    let offsets: Vec<i64> = config
        .overrides()
        .map(|(_, over)| over.offset().get())
        .collect();
    assert_eq!(offsets, [-50, 0, -1]);
}

#[test_case(r#"{"start_offsets":{"3":null}}"# ; "start offset")]
#[test_case(r#"{"candidate_solo_limits":{"3":null}}"# ; "candidate solo limit")]
fn null_offsets_rejected(text: &str) {
    assert!(matches!(
        Config::parse(text).unwrap_err(),
        ConfigError::Syntax(_)
    ));
}

#[test_case("start_offsets" ; "start offsets")]
#[test_case("candidate_solo_limits" ; "candidate solo limits")]
fn duplicate_core_keys_rejected(table: &str) {
    let text = format!(r#"{{"{table}":{{"3":-10,"3":-20}}}}"#);
    let err = Config::parse(&text).unwrap_err();
    assert!(
        matches!(err, ConfigError::DuplicateCore { core, .. } if core == self::core(3)),
        "{err}"
    );
}

#[test]
fn a_core_with_both_overrides_is_rejected() {
    let err = Config::parse(r#"{"start_offsets":{"3":-30},"candidate_solo_limits":{"3":-36}}"#)
        .unwrap_err();
    assert!(
        matches!(err, ConfigError::CoreConflict(c) if c == core(3)),
        "{err}"
    );
    assert_eq!(
        err.to_string(),
        "start_offsets.3 and candidate_solo_limits.3: set at most one per core"
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
    assert!(TrialSeconds::new(0).is_err());
    assert!(TrialSeconds::try_from(86_401).is_err());
    assert_eq!(TrialSeconds::new(86_400).unwrap().get(), 86_400);
    assert!(R7BudgetSeconds::new(3).is_err());
    assert!(DeadEndThreshold::new(101).is_err());
    assert!(CheckingSchedule::new(Vec::new()).is_err());
    assert!(Backends::new(Some("relative".into()), None).is_err());
    assert!(Backends::new(Some("/abs".into()), Some("/abs/y".into())).is_ok());
}

#[test]
fn customised_values_survive() {
    let config = customised();
    assert_eq!(
        config.override_for(core(15)),
        Some(InitialOverride::Start(offset(0)))
    );
    assert_eq!(config.checking_trial().get(), 1);
    assert_eq!(config.checking_all_core().get(), 4);
    assert_eq!(config.evidence().miss().get(), 0.001);
    assert_eq!(config.evidence().rate().get(), 0.25);
    assert_eq!(config.evidence().trials().get(), 25);
    assert_eq!(
        config.backends().ycruncher(),
        Some(Path::new("/opt/y-cruncher"))
    );
}

#[test]
fn the_default_snapshot_writes_every_setting() {
    let value = serde_json::to_value(Config::default().snapshot()).unwrap();
    assert_eq!(
        value,
        json!({
            "start_offsets": {},
            "candidate_solo_limits": {},
            "durations": {
                "search_trial_s": 90,
                "short_trial_s": 120,
                "checking_trial_s": 120,
                "checking_idle_s": 900,
                "checking_all_core_s": 1200
            },
            "evidence": {"miss": 0.05, "rate": 0.5},
            "checking": {"cycle": [
                "R7","R7","R7","R2","R2","R2","R6","R5","R1","R1","R1",
                "R3","R3","R3","R4","R4","R4","R6"
            ]},
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
            "start_offsets": {"3": -10, "15": 0},
            "candidate_solo_limits": {"2": -36},
            "durations": {
                "search_trial_s": 60,
                "short_trial_s": 86400,
                "checking_trial_s": 1,
                "checking_idle_s": 7200,
                "checking_all_core_s": 4
            },
            "evidence": {"miss": 0.001, "rate": 0.25},
            "checking": {"cycle": ["R6", "R1", "R6"]},
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

#[test_case(|snapshot| snapshot.durations.search_trial_s = 0, "search_trial_s" ; "zero search trial")]
#[test_case(
    |snapshot| snapshot.durations.checking_all_core_s = 3,
    "checking_all_core_s" ;
    "short all-core check"
)]
#[test_case(|snapshot| snapshot.evidence.miss = 1.0, "miss" ; "certain miss")]
#[test_case(|snapshot| snapshot.checking.cycle.clear(), "checking.cycle" ; "empty cycle")]
#[test_case(
    |snapshot| snapshot.dead_ends.stray_crashes_in_a_row = 101,
    "stray_crashes_in_a_row" ;
    "stray threshold above range"
)]
#[test_case(
    |snapshot| snapshot.backends.mprime = Some("relative/path".into()),
    "mprime" ;
    "relative backend path"
)]
#[test_case(|snapshot| snapshot.backend_user = Some(String::new()), "backend_user" ; "empty backend user")]
#[test_case(
    |snapshot| {
        snapshot.start_offsets.set(core(4), offset(-5));
        snapshot.candidate_solo_limits.set(core(4), offset(-9));
    },
    "start_offsets.4" ;
    "start offset below its solo limit"
)]
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
        serde_json::from_str::<Config>(r#"{"durations":{"search_trial_s":0}}"#).unwrap_err();
    assert_eq!(
        error.to_string(),
        "durations.search_trial_s: 0 is not within [1, 86400]"
    );
}

#[test]
fn serde_value_preserves_the_rejected_value_and_range() {
    let error = serde_json::from_value::<Config>(serde_json::json!({
        "durations": { "search_trial_s": 0 }
    }))
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "durations.search_trial_s: 0 is not within [1, 86400]"
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

#[test]
fn the_default_schedule_is_full_and_a_short_one_is_not() {
    assert!(CheckingSchedule::default().is_full());
    let short = CheckingSchedule::new(vec![Regime::R1, Regime::R2]).unwrap();
    assert!(!short.is_full());
}

fn least_schedule() -> Vec<Regime> {
    use Regime::{R1, R2, R3, R4, R5, R6, R7};
    vec![R1, R1, R1, R2, R2, R2, R3, R4, R5, R6, R7, R7, R7]
}

#[test]
fn a_schedule_is_full_at_each_regime_s_least_count() {
    assert!(CheckingSchedule::new(least_schedule()).unwrap().is_full());
}

#[test]
fn a_schedule_stays_full_with_extra_steps() {
    use Regime::{R3, R5, R7};
    let mut padded = least_schedule();
    padded.extend([R3, R5, R7]);
    assert!(CheckingSchedule::new(padded).unwrap().is_full());
}

#[test_case(Regime::R1 ; "r1")]
#[test_case(Regime::R2 ; "r2")]
#[test_case(Regime::R3 ; "r3")]
#[test_case(Regime::R4 ; "r4")]
#[test_case(Regime::R5 ; "r5")]
#[test_case(Regime::R6 ; "r6")]
#[test_case(Regime::R7 ; "r7")]
fn a_schedule_one_step_short_of_a_regime_is_not_full(regime: Regime) {
    let mut one_short = least_schedule();
    let at = one_short.iter().position(|&step| step == regime).unwrap();
    one_short.remove(at);
    assert!(!CheckingSchedule::new(one_short).unwrap().is_full());
}
