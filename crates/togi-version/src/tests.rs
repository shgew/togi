use super::{VERSION, resolve_revision, rev, string, version};

#[test]
fn revision_is_the_stamp_or_dev() {
    for (explicit, want) in [
        (Some("release-revision"), "release-revision"),
        (Some("dev"), "dev"),
        (Some(""), "dev"),
        (None, "dev"),
    ] {
        assert_eq!(resolve_revision(explicit), want, "stamp {explicit:?}");
    }
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
