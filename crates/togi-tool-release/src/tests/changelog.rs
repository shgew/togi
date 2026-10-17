use jiff::Timestamp;
use test_case::test_case;

use super::*;
use crate::chain;

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

#[test_case("0.1.0", "### Added\n\n- A feature", "0.1.1", "No breaking changes (patch bump)." ; "pre-one added")]
#[test_case("0.1.9", "### Changed\n\n- **BREAKING** migration", "0.2.0", "Breaking changes:\n- **BREAKING** migration" ; "pre-one breaking")]
#[test_case("1.2.3", "### Added\n\n- A feature", "1.3.0", "Added entries since the previous release (minor bump)." ; "post-one added")]
#[test_case("1.2.3", "### Fixed\n\n- Repair", "1.2.4", "No breaking changes (patch bump)." ; "post-one fixed")]
#[test_case("1.2.3", "### Fixed\n\n- **BREAKING** state reset", "2.0.0", "Breaking changes:\n- **BREAKING** state reset" ; "post-one breaking")]
fn bumps(version: &str, body: &str, next: &str, reason: &str) {
    assert_eq!(
        bump(version, body).unwrap(),
        (next.to_owned(), reason.to_owned())
    );
}

#[test_case("01.2.3", "invalid semantic version \"01.2.3\"" ; "leading zero")]
#[test_case(
    "9999999999999999999999999999999999999999.2.3",
    "parse version \"9999999999999999999999999999999999999999.2.3\": number too large to fit in target type" ;
    "overflow"
)]
#[test_case("18446744073709551615.2.3", "version \"18446744073709551615.2.3\" has no next version" ; "no next version")]
fn bump_refuses(version: &str, want: &str) {
    let body = "### Changed\n\n- **BREAKING** Reset.";
    assert_eq!(chain(&bump(version, body).unwrap_err()), want);
}

#[test]
fn rewrite_and_notes() {
    let before = "# Changelog\n\n## [0.1.0] - 2026-09-01\n\n### Fixed\n\n- Old behavior ([#2]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#2]: https://forge.example/o/r/pull/2\n[#40]: https://forge.example/o/r/pull/40\n";
    let updated = rewrite_changelog(
        before,
        "0.1.1",
        "### Added\n\n- New option ([#34]).\n- Old link ([#40]).",
        &[34, 40],
        "https://forge.example/o/r/",
        at("2026-09-26T00:30:00+01:00"),
    )
    .unwrap();
    let want = "# Changelog\n\n## [0.1.1] - 2026-09-25\n\n### Added\n\n- New option ([#34]).\n- Old link ([#40]).\n\n## [0.1.0] - 2026-09-01\n\n### Fixed\n\n- Old behavior ([#2]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[0.1.1]: https://forge.example/o/r/releases/tag/v0.1.1\n\n[#2]: https://forge.example/o/r/pull/2\n[#34]: https://forge.example/o/r/pull/34\n[#40]: https://forge.example/o/r/pull/40\n";
    assert_eq!(updated, want);
    let section = section_named(&updated, "0.1.1").unwrap().unwrap();
    assert_eq!(section.date, Some("2026-09-25"));
    assert_eq!(entries(section.body).len(), 2);
    assert_eq!(
        release_notes(&updated, section).unwrap(),
        "### Added\n\n- New option ([#34]).\n- Old link ([#40]).\n\n[#34]: https://forge.example/o/r/pull/34\n\n[#40]: https://forge.example/o/r/pull/40"
    );
}

#[test_case(
    "## [1.2.0] - 2026-09-24\n\n- Change ([#8]) and ([#3]).\n\n[#3]: https://forge.example/pulls/3\n[#8]: https://forge.example/pulls/8\n[#9]: https://forge.example/pulls/9\n",
    "1.2.0",
    "- Change ([#8]) and ([#3]).",
    "- Change ([#8]) and ([#3]).\n\n[#3]: https://forge.example/pulls/3\n\n[#8]: https://forge.example/pulls/8" ;
    "references"
)]
#[test_case(
    "## [1.2.1] - 2026-09-25\n\n### Fixed\n\n- Repair.\n",
    "1.2.1",
    "### Fixed\n\n- Repair.",
    "### Fixed\n\n- Repair." ;
    "no references"
)]
#[test_case(
    "## [1.2.2] - 2026-09-25\n\n- See [the guide][guide] and [source].\n\n[guide]: https://forge.example/guide\n[source]: https://forge.example/source\n[unused]: https://forge.example/unused\n",
    "1.2.2",
    "- See [the guide][guide] and [source].",
    "- See [the guide][guide] and [source].\n\n[guide]: https://forge.example/guide\n\n[source]: https://forge.example/source" ;
    "named links"
)]
#[test_case(
    "## [1.2.3] - 2026-09-25\n\n- See [guide](https://forge.example/guide).\n\n[guide]: https://forge.example/unrelated\n",
    "1.2.3",
    "- See [guide](https://forge.example/guide).",
    "- See [guide](https://forge.example/guide)." ;
    "inline link is not a reference"
)]
fn release_sections(changelog: &str, version: &str, body: &str, notes: &str) {
    let section = section_named(changelog, version).unwrap().unwrap();
    assert_eq!(section.body.trim(), body);
    assert_eq!(release_notes(changelog, section).unwrap(), notes);
}

#[test_case(
    "# Changelog\n",
    "# Changelog\n\n## [0.1.0] - 2026-09-25\n\n- New ([#4]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#4]: https://forge.example/o/r/pull/4\n" ;
    "header only"
)]
#[test_case(
    "# Changelog\n\n[#1]: https://forge.example/o/r/issues/1\n",
    "# Changelog\n\n## [0.1.0] - 2026-09-25\n\n- New ([#4]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#1]: https://forge.example/o/r/issues/1\n[#4]: https://forge.example/o/r/pull/4\n" ;
    "existing definitions"
)]
fn rewrite_first_release(input: &str, want: &str) {
    let got = rewrite_changelog(
        input,
        "0.1.0",
        "- New ([#4]).",
        &[4],
        "https://forge.example/o/r",
        at("2026-09-25T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(got, want);
}

#[test]
fn rewrite_refuses_existing_version() {
    let error = rewrite_changelog(
        "# Changelog\n\n## [1.2.3] - 2026-09-25\n",
        "1.2.3",
        "- Change.",
        &[],
        "https://forge.example/o/r",
        at("2026-09-25T00:00:00Z"),
    )
    .unwrap_err();
    assert_eq!(chain(&error), "version [1.2.3] already exists");
}
