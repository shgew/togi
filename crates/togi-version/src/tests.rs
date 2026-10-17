use test_case::test_case;

use super::{VERSION, resolve_revision, rev, string, version};

#[test_case(Some("release-revision"), "release-revision"; "a release stamp")]
#[test_case(Some("dev"), "dev"; "an explicit dev")]
#[test_case(Some(""), "dev"; "an empty stamp")]
#[test_case(None, "dev"; "no stamp")]
fn revision_is_the_stamp_or_dev(explicit: Option<&'static str>, want: &str) {
    assert_eq!(resolve_revision(explicit), want);
}

#[test]
fn version_is_the_trimmed_content_of_version_txt() {
    assert_eq!(version(), VERSION.trim());
    assert!(!version().is_empty());
}

#[test]
fn string_joins_version_and_revision_with_a_plus() {
    assert_eq!(string(), format!("{}+{}", version(), rev()));
}

#[test]
fn local_build_reports_dev_and_a_stamped_build_its_stamp() {
    match option_env!("TOGI_REV") {
        Some(stamp) if !stamp.is_empty() => assert_eq!(rev(), stamp),
        _ => assert_eq!(rev(), "dev"),
    }
}
