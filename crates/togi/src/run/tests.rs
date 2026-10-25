use clap::Parser;
use test_case::test_case;

use super::Args;

#[derive(Parser)]
struct Parsed {
    #[command(flatten)]
    run: Args,
}

fn parse(args: &[&str]) -> Result<Args, clap::Error> {
    Parsed::try_parse_from(std::iter::once("run").chain(args.iter().copied()))
        .map(|parsed| parsed.run)
}

#[test_case("--until" ; "until")]
#[test_case("--cycles" ; "cycles")]
#[test_case("--first-result" ; "first result")]
fn a_retired_flag_is_unknown(flag: &str) {
    let error = parse(&[flag]).unwrap_err().to_string();
    assert!(
        error.contains(&format!("unexpected argument '{flag}'")),
        "{error}"
    );
}

#[test]
fn the_old_cycle_flag_is_unknown() {
    let error = parse(&["--laps", "1"]).unwrap_err().to_string();
    assert!(error.contains("unexpected argument '--laps'"), "{error}");
}
