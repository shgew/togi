use clap::Parser;

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

#[test]
fn the_cycle_limit_is_endless_by_default_and_takes_a_positive_count() {
    assert_eq!(parse(&[]).unwrap().cycles, None);
    assert_eq!(
        parse(&["--cycles", "1"]).unwrap().cycles.map(u32::from),
        Some(1)
    );
    assert_eq!(
        parse(&["--cycles=3"]).unwrap().cycles.map(u32::from),
        Some(3)
    );
}

#[test]
fn a_cycle_limit_that_is_not_a_positive_count_is_refused() {
    for value in ["0", "-1", "many"] {
        let error = parse(&["--cycles", value]).unwrap_err().to_string();
        assert!(
            error.contains("must be a positive integer"),
            "{value}: {error}"
        );
    }
}

#[test]
fn the_first_result_stands_alone() {
    let args = parse(&["--first-result"]).unwrap();
    assert!(args.first_result);
    assert_eq!(args.cycles, None);
    assert!(parse(&["--first-result", "--cycles", "1"]).is_err());
}

#[test]
fn the_old_cycle_flag_is_unknown() {
    let error = parse(&["--laps", "1"]).unwrap_err().to_string();
    assert!(error.contains("unexpected argument '--laps'"), "{error}");
}
