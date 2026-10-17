use test_case::test_case;

use super::*;
use crate::chain;

fn args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn options() -> Command {
    Command::Report(Options {
        lcov: "t.info".into(),
        build_dir: "target/debug".into(),
        base: "origin/main".to_owned(),
    })
}

#[test_case("--lcov t.info --build-dir target/debug --base origin/main" ; "double dashes")]
#[test_case("-lcov t.info -build-dir target/debug -base origin/main" ; "single dashes")]
#[test_case("--base=origin/main --lcov=t.info --build-dir=target/debug" ; "inline values in any order")]
#[test_case("--lcov t.info --build-dir target/debug --base origin/main --" ; "a trailing terminator")]
fn flags(text: &str) {
    assert_eq!(parse(&args(text)).unwrap(), options());
}

#[test_case("-h" ; "short")]
#[test_case("--help" ; "long")]
fn help(text: &str) {
    assert_eq!(parse(&args(text)).unwrap(), Command::Help);
}

#[test_case("--lcov t.info --build-dir target/debug", "supply --lcov, --build-dir and --base, and no positional arguments" ; "no base")]
#[test_case("--lcov t.info --base main", "supply --lcov, --build-dir and --base, and no positional arguments" ; "no build directory")]
#[test_case("--lcov= --build-dir d --base main", "supply --lcov, --build-dir and --base, and no positional arguments" ; "an empty trace")]
#[test_case("--lcov t.info --build-dir d --base main extra", "supply --lcov, --build-dir and --base, and no positional arguments" ; "a positional argument")]
#[test_case("--lcov t.info --build-dir d --base main -- extra", "supply --lcov, --build-dir and --base, and no positional arguments" ; "a positional argument after the terminator")]
#[test_case("--profile p --base main", "flag provided but not defined: --profile" ; "the removed Go profile flag")]
#[test_case("--lcov t.info --build-dir d --base", "flag needs an argument: --base" ; "a flag without its value")]
fn refusals(text: &str, want: &str) {
    assert_eq!(chain(&parse(&args(text)).unwrap_err()), want);
}
