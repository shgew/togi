//! A type decoded from a JSON object refuses serde's positional array form, even a correctly sized one.

use serde::de::DeserializeOwned;

use crate::*;

fn assert_object_only<T: DeserializeOwned + std::fmt::Debug>(object: &str, array: &str) {
    serde_json::from_str::<T>(object).unwrap();
    let error = serde_json::from_str::<T>(array).unwrap_err().to_string();
    assert!(error.contains("expected an object"), "{array}: {error}");
}

#[test]
fn core_info_refuses_a_positional_array() {
    assert_object_only::<CoreInfo>(r#"{"core":1,"ccd":0,"cpus":[2,3]}"#, "[1,0,[2,3]]");
}

#[test]
fn bios_context_refuses_a_positional_array() {
    assert_object_only::<BiosContext>(
        r#"{"bios_version":"1","board":"b","cpu_model":"c","microcode":"m","boost_limit_mhz":5}"#,
        r#"["1","b","c","m",5]"#,
    );
}

#[test]
fn pstore_record_refuses_a_positional_array() {
    assert_object_only::<PstoreRecord>(r#"{"path":"p","lines":["l"]}"#, r#"["p",["l"]]"#);
}

#[test]
fn load_schedule_refuses_a_sequence_with_the_tag_first() {
    assert_object_only::<LoadSchedule>(r#"{"kind":"random","seed":7}"#, r#"["random",7]"#);
    assert_object_only::<LoadSchedule>(r#"{"kind":"duty","pct":50}"#, r#"["duty",50]"#);
    assert_object_only::<LoadSchedule>(r#"{"kind":"random","seed":7}"#, r#"["random",{"seed":7}]"#);
}

#[test]
fn machine_types_keep_refusing_unknown_fields() {
    assert!(serde_json::from_str::<CoreInfo>(r#"{"core":1,"ccd":0,"cpus":[],"x":1}"#).is_err());
    assert!(serde_json::from_str::<PstoreRecord>(r#"{"path":"p","lines":[],"x":1}"#).is_err());
    assert!(serde_json::from_str::<LoadSchedule>(r#"{"kind":"random","seed":1,"x":1}"#).is_err());
}

#[test]
fn nested_machine_types_are_object_only_inside_a_container() {
    let error = serde_json::from_str::<Vec<CoreInfo>>("[[1,0,[2]]]").unwrap_err();
    assert!(error.to_string().contains("expected an object"), "{error}");
    let error = serde_json::from_str::<Option<LoadSchedule>>(r#"["random",1]"#).unwrap_err();
    assert!(error.to_string().contains("expected an object"), "{error}");
}
