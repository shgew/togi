//! Registration refusals over the shared synthetic builder in `registration_fixture.rs`. Its
//! source identities and scenario parameters are synthetic identities, not generated truth.

use serde_json::{Value, json};
use togi_simrun::day_night::successor::{RunProfile, Split, UnitMode};

use super::fixture::{self as builder, HASH, REVISION, scenario};
use super::*;

/// Synthetic registration scenarios, in the order tests index them.
pub(crate) const SCENARIOS: [&str; 23] = [
    "successor-edge-shallow",
    "successor-edge-deep",
    "successor-no-write-limit-failure",
    "successor-slope-flat",
    "successor-slope-steep",
    "successor-flattening-tail",
    "successor-flatter-dangerous-class",
    "successor-delayed-onset",
    "successor-thermal-history",
    "successor-hidden-daily",
    "successor-partial-only",
    "successor-single-core-binding",
    "successor-jointly-binding",
    "successor-r8-nonbinding",
    "successor-both-ccd-interaction",
    "successor-stock-background",
    "successor-single-ccd",
    "successor-moving-cap",
    "successor-firmware-change",
    "successor-non-cpu-correct",
    "successor-non-cpu-wrong",
    "successor-non-cpu-missing",
    "successor-within-ccd-heterogeneous",
];

fn index(name: &str) -> usize {
    SCENARIOS.iter().position(|item| *item == name).unwrap()
}

pub(crate) fn value() -> Value {
    builder::value(&SCENARIOS)
}

pub(crate) fn json() -> String {
    value().to_string()
}

pub(crate) fn registration() -> Registration {
    Registration::parse(json().as_bytes()).expect("fixture parses")
}

fn parse(value: &Value) -> Result<Registration, Error> {
    Registration::parse(value.to_string().as_bytes())
}

fn edit(path: &str, new: Value) -> Value {
    let mut doc = value();
    *doc.pointer_mut(path)
        .unwrap_or_else(|| panic!("fixture path {path}")) = new;
    doc
}

#[test]
fn accepts_the_synthetic_registration() {
    let registration = registration();
    assert_eq!(registration.lambda_per_hour, LAMBDA_PER_HOUR);
    assert_eq!(registration.criteria, SETTLED_CRITERIA);
    assert_eq!(registration.scenarios.len(), 23);
    assert_eq!(
        registration
            .policy("successor")
            .map(|p| p.identity.as_str()),
        Some("successor-identity")
    );
}

#[test]
fn seed_selection_follows_the_run_profile() {
    let registration = registration();
    let reduced = registration
        .seeds(Split::Dev, RunProfile::ReducedDev)
        .unwrap();
    assert_eq!(reduced, (20001..=20010).collect::<Vec<_>>());
    let dev = registration
        .seeds(Split::Dev, RunProfile::Registered)
        .unwrap();
    assert_eq!(
        (dev.first(), dev.last(), dev.len()),
        (Some(&20001), Some(&20200), 200)
    );
    let holdout = registration
        .seeds(Split::Holdout, RunProfile::Registered)
        .unwrap();
    assert_eq!(
        (holdout.first(), holdout.last()),
        (Some(&30001), Some(&30200))
    );
    assert!(
        registration
            .seeds(Split::Holdout, RunProfile::ReducedDev)
            .is_err()
    );
}

#[test]
fn cells_cover_scenarios_modes_slopes_and_seeds() {
    let registration = registration();
    let cells = registration
        .cells(Split::Dev, RunProfile::ReducedDev)
        .unwrap();
    assert_eq!(cells.len(), 23 * 10 * 2);
    assert!(
        cells
            .iter()
            .all(|cell| cell.unit_mode == UnitMode::CcdUniform)
    );
    let first = &cells[0];
    assert_eq!(
        (first.scenario.as_str(), first.seed, first.slope),
        ("successor-edge-shallow", 20001, 1.5)
    );
}

#[test]
fn explicit_scenario_modes_narrow_the_applicable_modes() {
    let mut registration = registration();
    let scenario = &mut registration.scenarios[0];
    scenario.modes = Some(vec![UnitMode::CcdUniform]);
    let scenario = registration.scenarios[0].clone();
    assert_eq!(registration.modes(&scenario), [UnitMode::CcdUniform]);
}

#[test]
fn fractions_are_exact() {
    let ninety = Fraction {
        numerator: 90,
        denominator: 100,
    };
    assert!(ninety.met_by(180, 200));
    assert!(!ninety.met_by(179, 200));
    assert!(!ninety.met_by(0, 0));
    assert_eq!(
        Fraction {
            numerator: 80,
            denominator: 100
        }
        .ceil_of(33),
        27
    );
}

#[test]
fn hashes_are_lower_hex_sha256() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn refuses_non_objects_and_unknown_fields() {
    assert!(Registration::parse(b"[]").is_err());
    assert!(Registration::parse(b"null").is_err());
    let mut doc = value();
    doc["extra"] = json!(1);
    assert!(parse(&doc).is_err());
    let mut doc = value();
    doc["calendar"]["extra"] = json!(1);
    assert!(parse(&doc).is_err());
    let mut doc = value();
    doc["scenarios"][0] = json!([]);
    assert!(parse(&doc).is_err());
    let mut doc = value();
    doc["scenarios"][0]["extra"] = json!(1);
    assert!(parse(&doc).is_err());
}

#[test_case::test_case("/scenarios/0/topology", json!({"ccds": 0, "cores_per_ccd": 8}); "no ccds")]
#[test_case::test_case("/scenarios/0/topology", json!({"ccds": 3, "cores_per_ccd": 8}); "too many cores")]
#[test_case::test_case("/scenarios/0/daily_mix", json!([{"class": "compute", "seconds": 100}]); "short mix")]
#[test_case::test_case("/scenarios/0/daily_mix", json!([]); "empty mix")]
#[test_case::test_case("/scenarios/0/epochs", json!([]); "no epochs")]
#[test_case::test_case("/scenarios/0/epochs/0/start_day", json!(2); "late first epoch")]
#[test_case::test_case("/scenarios/0/modes", json!([]); "empty scenario modes")]
#[test_case::test_case("/scenarios/0/modes", json!(["per-core"]); "unregistered scenario mode")]
#[test_case::test_case("/scenarios/0/modes", json!(["ccd-uniform", "ccd-uniform"]); "duplicate scenario mode")]
#[test_case::test_case("/calendar/night_weekdays", json!([4, 2, 0]); "unordered weekdays")]
#[test_case::test_case("/calendar/night_weekdays", json!([0, 2, 7]); "weekday out of range")]
#[test_case::test_case("/resources/jobs", json!(0); "no jobs")]
#[test_case::test_case("/lambda_per_hour/denominator", json!(0); "zero denominator")]
fn refuses_malformed_dimensions(path: &str, new: Value) {
    let edited = edit(path, new);
    assert!(
        parse(&edited).is_err(),
        "{path} = {}",
        edited.pointer(path).unwrap()
    );
}

#[test]
fn refuses_wrong_transition_schedules() {
    let index = index("successor-firmware-change");
    let path = format!("/scenarios/{index}/epochs/1/start_day");
    assert!(matches!(
        parse(&edit(&path, json!(200))),
        Err(Error::Invalid(_))
    ));
    let path = format!("/scenarios/{index}/transition");
    assert!(parse(&edit(&path, json!("fixed"))).is_err());
}

/// Sets `path` to a raw JSON number token that `serde_json` cannot represent as a finite f64.
fn parse_with_raw(path: &str, raw: &str) -> Result<Registration, Error> {
    const SENTINEL: &str = "raw-number-sentinel";
    let text = edit(path, json!(SENTINEL)).to_string();
    let quoted = format!("\"{SENTINEL}\"");
    assert_eq!(text.matches(&quoted).count(), 1, "{path}");
    Registration::parse(text.replace(&quoted, raw).as_bytes())
}

#[test_case::test_case("/slopes/0"; "slope")]
#[test_case::test_case("/tolerance/admissibility"; "admissibility tolerance")]
#[test_case::test_case("/scenarios/0/parameters/0/value"; "scenario parameter")]
fn refuses_overflowing_numbers(path: &str) {
    assert!(parse_with_raw(path, "1e999").is_err(), "{path}");
}

#[test]
fn refuses_null_slope_and_out_of_range_tolerances() {
    assert!(matches!(
        parse(&edit("/slopes/0", json!(null))),
        Err(Error::Decode(_))
    ));
    assert!(parse(&edit("/tolerance/admissibility", json!(-1e-9))).is_err());
    assert!(parse(&edit("/tolerance/hours", json!(0.5))).is_err());
}

#[test_case::test_case("/version", json!(2); "version")]
#[test_case::test_case("/lambda_per_hour/denominator", json!(2000); "lambda")]
#[test_case::test_case("/slopes", json!([1.5]); "one slope")]
#[test_case::test_case("/slopes", json!([1.5, 2.5]); "other slope")]
#[test_case::test_case("/seeds/dev/first", json!(20002); "dev first")]
#[test_case::test_case("/seeds/dev/count", json!(199); "dev count")]
#[test_case::test_case("/seeds/holdout/first", json!(30002); "holdout first")]
#[test_case::test_case("/seeds/holdout/count", json!(10); "holdout count")]
#[test_case::test_case("/calendar/days", json!(365); "days")]
#[test_case::test_case("/calendar/daily_active_hours", json!(6); "daily hours")]
#[test_case::test_case("/calendar/night_weekdays", json!([0, 2]); "night weekdays")]
#[test_case::test_case("/calendar/firmware_change_day", json!(180); "firmware day")]
#[test_case::test_case("/criteria/held_after_finish_fraction/numerator", json!(90); "held fraction")]
#[test_case::test_case("/criteria/useful_depth_of_reference/numerator", json!(70); "useful depth")]
#[test_case::test_case("/criteria/min_unit_depth", json!(4); "min depth")]
#[test_case::test_case("/criteria/settled_horizon_day", json!(800); "settled horizon")]
#[test_case::test_case("/criteria/benign_median_settled_day", json!(200); "benign median")]
#[test_case::test_case("/criteria/retention_max_loss", json!(2); "retention loss")]
#[test_case::test_case("/criteria/allowance_limit", json!(3); "allowance")]
#[test_case::test_case("/criteria/clean_hold_hours", json!(25); "hold hours")]
#[test_case::test_case("/criteria/safety_grace_days", json!(1); "grace")]
#[test_case::test_case("/criteria/operational_max_depth", json!(50); "operational depth")]
#[test_case::test_case("/run_profiles/0/seeds", json!({"kind": "first", "count": 20}); "reduced seeds")]
#[test_case::test_case("/run_profiles/0/gate_verdict", json!(true); "reduced verdict")]
#[test_case::test_case("/run_profiles/1/splits", json!(["dev"]); "registered splits")]
#[test_case::test_case("/run_profiles/1/seeds", json!({"kind": "first", "count": 100}); "registered seeds")]
#[test_case::test_case("/budgets/reduced_dev_seconds", json!(3600); "reduced budget")]
#[test_case::test_case("/budgets/registered_seconds", json!(86400); "registered budget")]
#[test_case::test_case("/budgets/holdout_seconds", json!(100); "holdout budget")]
#[test_case::test_case("/holdout/confirmations_per_slope", json!(2); "confirmations")]
#[test_case::test_case("/holdout/spent_when_started", json!(false); "spent")]
#[test_case::test_case("/holdout/inspection_before_freeze", json!(true); "inspection")]
#[test_case::test_case("/continuation/holdout_resumable", json!(true); "holdout resumable")]
#[test_case::test_case("/scenarios/2/unit_depth_floor", json!(40); "depth floor")]
#[test_case::test_case("/scenarios/2/benign", json!(false); "benign flag")]
#[test_case::test_case("/scenarios/15/retention", json!(false); "retention flag")]
fn refuses_changed_settled_values(path: &str, new: Value) {
    let edited = edit(path, new);
    assert!(
        parse(&edited).is_err(),
        "{path} = {}",
        edited.pointer(path).unwrap()
    );
}

#[test]
fn refuses_missing_scenarios_and_bad_applicability() {
    // A scenario carrying no required role may be absent: nothing is matched by name.
    let mut doc = value();
    doc["scenarios"]
        .as_array_mut()
        .unwrap()
        .remove(index("successor-edge-shallow"));
    assert!(parse(&doc).is_ok());
    // Removing a role-bearing scenario is refused.
    let mut doc = value();
    doc["scenarios"]
        .as_array_mut()
        .unwrap()
        .remove(index("successor-moving-cap"));
    assert!(parse(&doc).is_err());
    let mut doc = value();
    doc["scenarios"][1] = scenario("successor-edge-shallow");
    assert!(parse(&doc).is_err());
    let adversarial = index("successor-hidden-daily");
    assert!(
        parse(&edit(
            &format!("/scenarios/{adversarial}/benign"),
            json!(true)
        ))
        .is_err()
    );
    let mut doc = value();
    doc["scenarios"].as_array_mut().unwrap().pop();
    assert!(parse(&doc).is_err());
}

#[test_case::test_case("/scenarios/0/name", json!(""); "scenario name")]
#[test_case::test_case("/scenarios/0/description", json!("  "); "scenario description")]
#[test_case::test_case("/scenarios/0/epochs/0/id", json!(""); "epoch id")]
#[test_case::test_case("/sources/policies/0/name", json!(""); "policy name")]
#[test_case::test_case("/sources/policies/1/name", json!("successor"); "duplicate policy name")]
#[test_case::test_case("/sources/policies/1/identity", json!("successor-identity"); "duplicate policy identity")]
#[test_case::test_case("/sources/policies/0/source/revision", json!("abc"); "short source revision")]
#[test_case::test_case("/sources/policies/0/source/sha256", json!(HASH.to_uppercase()); "uppercase source hash")]
#[test_case::test_case("/sources/policies/0/parameters_sha256", json!(""); "empty parameters hash")]
#[test_case::test_case("/sources/policies/0/parameters", json!([]); "no parameters")]
#[test_case::test_case("/sources/simulator_revision", json!(""); "empty simulator revision")]
#[test_case::test_case("/sources/report_revision", json!("main"); "moving report revision")]
#[test_case::test_case("/sources/binary_sha256", json!("short"); "short binary hash")]
#[test_case::test_case("/sources/policies/1/role", json!("candidate"); "two candidates")]
#[test_case::test_case("/sources/policies/0/role", json!("comparator"); "no candidate")]
#[test_case::test_case("/unit_modes", json!([]); "no unit modes")]
#[test_case::test_case("/unit_modes/0/grouping", json!(""); "empty grouping")]
#[test_case::test_case("/commands/reduced_dev", json!("just bench-day-night --run-profile reduced-dev --split dev"); "reduced command without registration")]
#[test_case::test_case("/commands/registered", json!("make"); "foreign command")]
#[test_case::test_case("/holdout/authorized_by", json!(""); "empty authority")]
#[test_case::test_case("/holdout/command", json!("just bench-day-night --registration R"); "holdout command without selectors")]
#[test_case::test_case("/streams/domains", json!([]); "no stream domains")]
#[test_case::test_case("/streams/domains", json!([{"name": "a", "tag": "T"}, {"name": "a", "tag": "U"}]); "duplicate stream domain")]
#[test_case::test_case("/continuation/dev_rule", json!(""); "empty continuation rule")]
#[test_case::test_case("/notes", json!([""]); "empty note")]
fn refuses_empty_or_duplicate_identities(path: &str, new: Value) {
    let edited = edit(path, new);
    assert!(
        parse(&edited).is_err(),
        "{path} = {}",
        edited.pointer(path).unwrap()
    );
}

#[test]
fn accepts_pinned_binary_and_input_hashes() {
    let mut doc = value();
    doc["sources"]["binary_sha256"] = json!(HASH);
    doc["sources"]["input_sha256"] = json!(HASH);
    let registration = parse(&doc).unwrap();
    assert_eq!(registration.sources.binary_sha256.as_deref(), Some(HASH));
}

fn per_core() -> Value {
    let mut doc = value();
    doc["id"] = json!("day-night-successor-per-core");
    doc["unit_modes"] =
        json!([{"mode": "per-core", "grouping": "one unit per physical core; lambda/n each"}]);
    doc
}

#[test]
fn accepts_a_distinct_per_core_registration() {
    let registration = parse(&per_core()).unwrap();
    let scenario = registration.scenarios[0].clone();
    assert_eq!(registration.modes(&scenario), [UnitMode::PerCore]);
    let cells = registration
        .cells(Split::Dev, RunProfile::ReducedDev)
        .unwrap();
    assert!(cells.iter().all(|cell| cell.unit_mode == UnitMode::PerCore));
}

#[test]
fn refuses_mixed_or_misnamed_mode_registrations() {
    let mut mixed = per_core();
    mixed["unit_modes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"mode": "ccd-uniform", "grouping": "g"}));
    assert!(parse(&mixed).is_err());
    // The id carries no mode: a per-core document under the id the fixture uses for ccd-uniform
    // is still exactly one mode and is accepted.
    let mut current = per_core();
    current["id"] = json!("day-night-successor");
    assert_eq!(
        parse(&current).unwrap().unit_modes[0].mode,
        UnitMode::PerCore
    );
}

#[test_case::test_case("day-night-successor"; "fixture id")]
#[test_case::test_case("ccd-uniform-2031-trial"; "arbitrary ccd id")]
#[test_case::test_case("x"; "short id")]
fn ccd_uniform_accepts_any_nonempty_id(id: &str) {
    let registration = parse(&edit("/id", json!(id))).unwrap();
    assert_eq!(registration.id, id);
    assert_eq!(registration.unit_modes[0].mode, UnitMode::CcdUniform);
}

#[test]
fn refuses_blank_ids() {
    for id in ["", "  "] {
        assert!(parse(&edit("/id", json!(id))).is_err(), "{id:?}");
    }
}

#[test]
fn the_runner_revision_is_a_required_pin() {
    let mut doc = value();
    doc["sources"]
        .as_object_mut()
        .unwrap()
        .remove("runner_revision");
    assert!(matches!(parse(&doc), Err(Error::Decode(_))));
    for bad in [
        "",
        "main",
        "abc",
        "0123456789ABCDEF0123456789ABCDEF01234567",
    ] {
        let edited = edit("/sources/runner_revision", json!(bad));
        assert!(matches!(parse(&edited), Err(Error::Invalid(_))), "{bad:?}");
    }
    let registration = parse(&value()).unwrap();
    assert_eq!(registration.sources.runner_revision, REVISION);
}

#[test]
fn pinned_and_pending_policy_sources_are_typed() {
    let registration = parse(&value()).unwrap();
    assert_eq!(
        registration.policy("successor").unwrap().source,
        SourceIdentity::Pinned {
            revision: REVISION.into(),
            sha256: HASH.into(),
        }
    );
    let pending = edit("/sources/policies/0/source", json!({"kind": "pending"}));
    let registration = parse(&pending).unwrap();
    let source = &registration.policy("successor").unwrap().source;
    assert_eq!(*source, SourceIdentity::Pending {});
    assert!(source.is_pending());
    assert!(!registration.policy("shipped").unwrap().source.is_pending());
}

#[test_case::test_case(json!({"kind": "pending", "revision": REVISION}); "pending with a pin")]
#[test_case::test_case(json!({"kind": "pinned"}); "pinned without fields")]
#[test_case::test_case(json!({"kind": "pinned", "revision": REVISION}); "pinned without hash")]
#[test_case::test_case(json!({"kind": "pinned", "revision": "abc", "sha256": HASH}); "short revision")]
#[test_case::test_case(json!({"kind": "pinned", "revision": REVISION, "sha256": HASH.to_uppercase()}); "uppercase hash")]
#[test_case::test_case(json!({"kind": "pinned", "revision": "", "sha256": ""}); "empty pin is not pending")]
#[test_case::test_case(json!({"kind": "pinned", "revision": REVISION, "sha256": HASH, "extra": 1}); "unknown field")]
#[test_case::test_case(json!({"kind": "unknown"}); "unknown kind")]
#[test_case::test_case(json!({}); "no kind")]
#[test_case::test_case(json!(""); "sentinel string")]
#[test_case::test_case(json!(null); "null")]
fn refuses_malformed_source_identities(source: Value) {
    assert!(parse(&edit("/sources/policies/0/source", source)).is_err());
}

#[test_case::test_case("source_revision", json!(REVISION); "old revision field")]
#[test_case::test_case("source_sha256", json!(HASH); "old hash field")]
fn refuses_the_removed_policy_source_fields(field: &str, entry: Value) {
    let mut doc = value();
    doc["sources"]["policies"][0][field] = entry;
    assert!(matches!(parse(&doc), Err(Error::Decode(_))));
}

#[test]
fn refuses_a_policy_without_a_source() {
    let mut doc = value();
    doc["sources"]["policies"][0]
        .as_object_mut()
        .unwrap()
        .remove("source");
    assert!(matches!(parse(&doc), Err(Error::Decode(_))));
}

#[test]
fn refuses_missing_required_scenario_roles() {
    let settled = |doc: &Value, needle: &str| matches!(parse(doc), Err(Error::Settled(message)) if message.contains(needle));
    let only = json!([{"id": "only", "start_day": 1, "context": "ctx", "description": "d"}]);
    // R3-1: moving-cap switched to fixed with one initial epoch is otherwise a valid scenario;
    // the moving role is what is missing.
    let mut doc = value();
    let moving = index("successor-moving-cap");
    doc["scenarios"][moving]["transition"] = json!("fixed");
    doc["scenarios"][moving]["epochs"] = only.clone();
    assert!(settled(&doc, "moving"));
    let mut doc = value();
    let firmware = index("successor-firmware-change");
    doc["scenarios"][firmware]["transition"] = json!("fixed");
    doc["scenarios"][firmware]["epochs"] = only;
    assert!(settled(&doc, "firmware"));
    let mut doc = value();
    for scenario in doc["scenarios"].as_array_mut().unwrap() {
        scenario["coverage"] = json!("covered");
    }
    assert!(settled(&doc, "adversarial"));
}

#[test]
fn roles_follow_declared_flags_not_names() {
    // Renaming every scenario leaves a valid registration: nothing is matched by name.
    let mut doc = value();
    for (position, scenario) in doc["scenarios"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        scenario["name"] = json!(format!("renamed-{position}"));
    }
    assert_eq!(parse(&doc).unwrap().scenarios.len(), SCENARIOS.len());
}

#[test]
fn the_depth_floor_is_one_benign_scenario_at_the_settled_value() {
    let floor = index("successor-no-write-limit-failure");
    let mut doc = value();
    doc["scenarios"][floor]["unit_depth_floor"] = Value::Null;
    assert!(matches!(
        parse(&doc),
        Err(Error::Settled("unit depth floor"))
    ));
    let mut doc = value();
    doc["scenarios"][index("successor-edge-deep")]["unit_depth_floor"] = json!(44);
    assert!(matches!(
        parse(&doc),
        Err(Error::Settled("unit depth floor"))
    ));
    let mut doc = value();
    doc["scenarios"][floor]["unit_depth_floor"] = json!(43);
    assert!(matches!(
        parse(&doc),
        Err(Error::Settled("unit depth floor"))
    ));
    let mut doc = value();
    doc["scenarios"][floor]["unit_depth_floor"] = Value::Null;
    doc["scenarios"][index("successor-slope-flat")]["unit_depth_floor"] = json!(44);
    assert!(matches!(
        parse(&doc),
        Err(Error::Settled("unit depth floor"))
    ));
}

#[test]
fn seed_ranges_reject_unrepresentable_hand_built_fields() {
    assert!(SeedRange { first: 1, count: 0 }.values().is_err());
    assert!(
        SeedRange {
            first: u64::MAX,
            count: 2
        }
        .values()
        .is_err()
    );
    assert_eq!(SeedRange { first: 5, count: 2 }.values().unwrap(), 5..=6);
}

#[test]
fn the_input_digest_is_the_hash_of_the_registration_with_a_null_input_pin() {
    let mut cloned = registration();
    cloned.sources.input_sha256 = None;
    let expected = sha256_hex(&serde_json::to_vec(&cloned).unwrap());
    let mut pinned = registration();
    pinned.sources.input_sha256 = Some(HASH.into());
    assert_eq!(input_sha256(&pinned).unwrap(), expected);
    assert_eq!(input_sha256(&registration()).unwrap(), expected);
}

#[test]
fn the_input_pin_does_not_change_the_digest_but_other_fields_do() {
    let base = input_sha256(&registration()).unwrap();
    let mut other_pin = registration();
    other_pin.sources.input_sha256 = Some("f".repeat(64));
    assert_eq!(input_sha256(&other_pin).unwrap(), base);
    let mut binary = registration();
    binary.sources.binary_sha256 = Some(HASH.into());
    assert_ne!(input_sha256(&binary).unwrap(), base);
    let mut scenario = registration();
    scenario.scenarios[0].description.push('!');
    assert_ne!(input_sha256(&scenario).unwrap(), base);
    let mut policy = registration();
    policy.sources.policies[1].parameters[0].value = 3.0;
    assert_ne!(input_sha256(&policy).unwrap(), base);
}
