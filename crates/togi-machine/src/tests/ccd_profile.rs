use super::*;
use test_case::test_case;

fn ccd(value: u8) -> CcdId {
    CcdId::try_from(value).unwrap()
}

fn offset(value: i64) -> TargetOffset {
    TargetOffset::new(value).unwrap()
}

fn profile(first: i64, second: i64) -> CcdProfile {
    CcdProfile::zero([ccd(0), ccd(1)])
        .unwrap()
        .with(ccd(0), offset(first))
        .with(ccd(1), offset(second))
}

#[test]
fn expands_every_core_to_its_ccd_offset() {
    let cores = profile(-10, -4).expand(&topology()).unwrap();
    assert_eq!(cores.len(), 16);
    for value in 0..16 {
        let expected = if value < 8 { -10 } else { -4 };
        assert_eq!(
            cores.get(core(value)),
            Some(offset(expected)),
            "core {value}"
        );
    }
    assert_eq!(
        CcdProfile::of_cores(&cores, &topology()),
        Some(profile(-10, -4))
    );
}

#[test]
fn a_nonuniform_ccd_is_not_a_profile() {
    let mut cores = profile(-10, -4).expand(&topology()).unwrap();
    cores.set(core(3), offset(-9));
    assert_eq!(CcdProfile::of_cores(&cores, &topology()), None);
}

#[test]
fn a_missing_core_is_not_a_profile() {
    let cores: PerCore<TargetOffset> = (0..15).map(|value| (core(value), offset(0))).collect();
    assert_eq!(CcdProfile::of_cores(&cores, &topology()), None);
}

#[test]
fn expanding_needs_every_ccd_of_the_topology() {
    let single = CcdProfile::zero([ccd(0)]).unwrap();
    assert_eq!(
        single.expand(&topology()),
        Err(CcdProfileError::Missing(ccd(1)))
    );
    let extra = CcdProfile::zero([ccd(0), ccd(1), ccd(2)]).unwrap();
    assert_eq!(
        extra.expand(&topology()),
        Err(CcdProfileError::Extra(ccd(2)))
    );
}

#[test_case(&profile(-10, -4), &profile(-10, -4), true; "equal")]
#[test_case(&profile(-11, -4), &profile(-10, -4), true; "deeper on one")]
#[test_case(&profile(-9, -5), &profile(-10, -4), false; "shallower on one")]
#[test_case(&profile(0, 0), &profile(-10, -4), false; "stock")]
fn compares_at_or_deeper_on_every_ccd(candidate: &CcdProfile, veto: &CcdProfile, expected: bool) {
    assert_eq!(candidate.at_or_deeper_than(veto), expected);
}

#[test]
fn serializes_as_an_object_keyed_by_ccd() {
    let json = serde_json::to_string(&profile(-10, -4)).unwrap();
    assert_eq!(json, r#"{"0":-10,"1":-4}"#);
    assert_eq!(
        serde_json::from_str::<CcdProfile>(&json).unwrap(),
        profile(-10, -4)
    );
    assert!(serde_json::from_str::<CcdProfile>("{}").is_err());
}

#[test]
fn digest_accepts_only_lowercase_sha256_hex() {
    let hex = "ab".repeat(32);
    assert_eq!(Digest::new(hex.clone()).unwrap().as_str(), hex);
    assert!(Digest::new("AB".repeat(32)).is_err());
    assert!(Digest::new("ab".repeat(31)).is_err());
    assert_eq!(Digest::from_bytes([0xab; 32]).as_str(), hex);
}
