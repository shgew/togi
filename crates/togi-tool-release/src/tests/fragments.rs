use std::collections::BTreeMap;
use std::fs;

use test_case::test_case;

use super::*;
use crate::chain;
use crate::changelog::bump;

fn entry(pr: u64, section: Section, text: &str) -> Entry {
    Entry {
        pr,
        section,
        text: text.to_owned(),
    }
}

#[test]
fn parses_fragment() {
    let got = parse_fragment(
        "42.md",
        "### Changed\n\n- **BREAKING** Reset the core.\n- Faster.\n\n### Fixed\n\n- A crash.\n",
    )
    .unwrap();
    assert_eq!(
        got,
        [
            entry(42, Section::Changed, "**BREAKING** Reset the core."),
            entry(42, Section::Changed, "Faster."),
            entry(42, Section::Fixed, "A crash."),
        ]
    );
}

#[test_case("A crash ([#42])." ; "reference")]
#[test_case("A crash [PR 42](https://github.com/shgew/togi/pull/42)." ; "inline link")]
#[test_case("A crash https://github.com/shgew/togi/pull/42." ; "bare URL")]
#[test_case("A crash https://github.com/other/project/pull/7#discussion_r1." ; "other repository")]
fn rejects_pull_request_links(text: &str) {
    let error = parse_fragment("42.md", &format!("### Fixed\n\n- {text}\n")).unwrap_err();
    assert_eq!(
        chain(&error),
        "42.md:3: entry must not link a pull request; the release adds ([#42])"
    );
}

#[test_case("A crash." ; "normal entry")]
#[test_case("See [docs](https://github.com/shgew/togi/blob/main/README.md)." ; "docs link")]
#[test_case("A crash [issue 42](https://github.com/shgew/togi/issues/42)." ; "issue link")]
fn accepts_other_links(text: &str) {
    let got = parse_fragment("42.md", &format!("### Fixed\n\n- {text}\n")).unwrap();
    assert_eq!(got, [entry(42, Section::Fixed, text)]);
}

#[test_case("fast-state.md", "### Fixed\n\n- A crash.\n", "fast-state.md: name must be the pull request number, as in 123.md" ; "slug name")]
#[test_case("042.md", "### Fixed\n\n- A crash.\n", "042.md: name must be the pull request number, as in 123.md" ; "leading zero")]
#[test_case("42.txt", "### Fixed\n\n- A crash.\n", "42.txt: name must be the pull request number, as in 123.md" ; "other extension")]
#[test_case("42.md", "\n", "42.md: no entries" ; "empty")]
#[test_case("42.md", "### Security\n\n- A hole.\n", "42.md:1: section \"Security\" is not one of Added, Changed, Removed, Fixed" ; "unknown section")]
#[test_case("42.md", "- A crash.\n", "42.md:1: entry before any ### section heading" ; "before heading")]
#[test_case("42.md", "### Fixed\n\n- A crash\n", "42.md:3: entry must end with a period" ; "no period")]
#[test_case("42.md", "### Fixed\n\n- .\n", "42.md:3: entry must end with a period" ; "bare period")]
#[test_case("42.md", "### Fixed\n\n- A crash ([#42]).\n", "42.md:3: entry must not link a pull request; the release adds ([#42])" ; "own link")]
#[test_case("42.md", "### Added\n\n### Fixed\n\n- A crash.\n", "42.md:3: ### Added has no entries" ; "empty section")]
#[test_case("42.md", "### Fixed\n\n- A crash.\n\n### Added\n", "42.md:6: ### Added has no entries" ; "empty last section")]
#[test_case("42.md", "### Fixed\n\n- A crash\n  on resume.\n", "42.md:3: entry must end with a period" ; "continuation")]
#[test_case("42.md", "### Fixed\n\nA crash.\n", "42.md:3: expected a ### section heading or a \"- \" entry on one line" ; "prose")]
fn rejects_fragment(name: &str, content: &str, want: &str) {
    assert_eq!(chain(&parse_fragment(name, content).unwrap_err()), want);
}

#[test]
fn assembles_release_section() {
    let files: BTreeMap<String, String> = [
        ("README.md", "Not a fragment."),
        (
            "9.md",
            "### Fixed\n\n- Ninth fix.\n\n### Added\n\n- Ninth feature.\n",
        ),
        (
            "12.md",
            "### Changed\n\n- Twelfth change.\n- **BREAKING** Twelfth reset.\n",
        ),
        (
            "10.md",
            "### Fixed\n\n- **BREAKING** Tenth fix.\n\n### Removed\n\n- Tenth removal.\n",
        ),
        ("3.md", "### Changed\n\n- Third change.\n"),
    ]
    .into_iter()
    .map(|(name, content)| (name.to_owned(), content.to_owned()))
    .collect();
    let (body, prs) = assemble(&parse_fragments(&files).unwrap());
    let want = "### Added\n\n- Ninth feature ([#9]).\n\n\
        ### Changed\n\n- **BREAKING** Twelfth reset ([#12]).\n- Third change ([#3]).\n- Twelfth change ([#12]).\n\n\
        ### Removed\n\n- Tenth removal ([#10]).\n\n\
        ### Fixed\n\n- **BREAKING** Tenth fix ([#10]).\n- Ninth fix ([#9]).";
    assert_eq!(body, want);
    assert_eq!(prs, [3, 9, 10, 12]);
    assert_eq!(bump("0.8.0", &body).unwrap().0, "0.9.0");
}

#[test_case(
    "See [crashes](../docs/howto.md#recording-a-crash-togi-did-not-observe) and [tuning](../docs/how-togi-tunes.md).",
    "See [crashes](docs/howto.md#recording-a-crash-togi-did-not-observe) and [tuning](docs/how-togi-tunes.md) ([#7])."
    ; "fragment-relative links"
)]
#[test_case("A chart ![cores](../docs/cores.svg).", "A chart ![cores](docs/cores.svg) ([#7])." ; "image")]
#[test_case(
    "See [docs](https://github.com/shgew/togi/blob/main/README.md) and [issue 42](https://github.com/shgew/togi/issues/42).",
    "See [docs](https://github.com/shgew/togi/blob/main/README.md) and [issue 42](https://github.com/shgew/togi/issues/42) ([#7])."
    ; "absolute links"
)]
#[test_case("Use `../togi` from [a](#usage).", "Use `../togi` from [a](#usage) ([#7])." ; "not a link destination")]
fn assembles_links_relative_to_changelog(text: &str, want: &str) {
    let files = BTreeMap::from([("7.md".to_owned(), format!("### Fixed\n\n- {text}\n"))]);
    let (body, _) = assemble(&parse_fragments(&files).unwrap());
    assert_eq!(body, format!("### Fixed\n\n- {want}"));
}

#[test]
fn real_fs_check_fragments_accepts_valid_directory() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("README.md"),
        "# Changes\n\nFree-form instructions.\n",
    )
    .unwrap();
    fs::write(dir.path().join("7.md"), "### Added\n\n- A feature.\n").unwrap();
    check_fragments(dir.path()).unwrap();
}

#[test]
fn real_fs_check_fragments_accepts_missing_directory() {
    let dir = tempfile::tempdir().unwrap();
    check_fragments(&dir.path().join("changes")).unwrap();
}

#[test]
fn real_fs_check_fragments_refuses_invalid_fragment() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("7.md"), "### Added\n\n- A feature\n").unwrap();
    let error = check_fragments(dir.path()).unwrap_err();
    assert_eq!(
        chain(&error),
        format!(
            "check {}: 7.md:3: entry must end with a period",
            dir.path().display()
        )
    );
}

#[test]
fn real_fs_check_fragments_refuses_subdirectory() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("7.md")).unwrap();
    assert_eq!(
        chain(&check_fragments(dir.path()).unwrap_err()),
        "7.md: unexpected directory"
    );
}

#[test]
fn real_fs_check_fragments_refuses_first_subdirectory_by_name() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["9.md", "3.md", "5.md"] {
        fs::create_dir(dir.path().join(name)).unwrap();
    }
    assert_eq!(
        chain(&check_fragments(dir.path()).unwrap_err()),
        "3.md: unexpected directory"
    );
}
