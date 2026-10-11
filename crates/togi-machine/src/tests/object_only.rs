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

/// The control: what a plain derive accepts and `object_only!` refuses, with serde's attributes kept.
mod mechanism {
    use serde::{Deserialize, Serialize};

    use crate::object_only;

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Plain {
        a: u8,
        b: Option<u8>,
    }

    object_only! {
        /// A documented definition.
        #[derive(Debug, PartialEq, Serialize)]
        #[serde(deny_unknown_fields)]
        struct Guarded {
            #[serde(rename = "first")]
            a: u8,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            b: Option<u8>,
        }

        #[derive(Debug, PartialEq, Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Tagged {
            One { n: u8 },
            Two { m: u8, #[serde(default)] extra: bool },
        }
    }

    #[test]
    fn a_plain_derive_takes_the_array_this_macro_refuses() {
        assert_eq!(
            serde_json::from_str::<Plain>("[1,2]").unwrap(),
            Plain { a: 1, b: Some(2) }
        );
        assert!(serde_json::from_str::<Guarded>("[1,2]").is_err());
        assert!(serde_json::from_str::<Guarded>("[1]").is_err());
    }

    #[test]
    fn serde_attributes_and_serialization_are_unchanged() {
        let guarded = serde_json::from_str::<Guarded>(r#"{"first":1}"#).unwrap();
        assert_eq!(guarded, Guarded { a: 1, b: None });
        assert_eq!(serde_json::to_string(&guarded).unwrap(), r#"{"first":1}"#);
        assert!(serde_json::from_str::<Guarded>(r#"{"first":1,"x":2}"#).is_err());
        assert!(serde_json::from_str::<Guarded>(r#"{"a":1}"#).is_err());
        assert_eq!(
            serde_json::from_str::<Tagged>(r#"{"kind":"two","m":3}"#).unwrap(),
            Tagged::Two { m: 3, extra: false }
        );
        assert_eq!(
            serde_json::to_string(&Tagged::One { n: 4 }).unwrap(),
            r#"{"kind":"one","n":4}"#
        );
        assert!(serde_json::from_str::<Tagged>(r#"["one",4]"#).is_err());
        assert!(serde_json::from_str::<Tagged>(r#"["two",3,true]"#).is_err());
    }

    #[test]
    fn the_trait_call_refuses_an_array_too() {
        let mut input = serde_json::Deserializer::from_str("[1,0,[]]");
        let error = crate::CoreInfo::deserialize(&mut input).unwrap_err();
        assert!(error.to_string().contains("expected an object"), "{error}");
    }
}
