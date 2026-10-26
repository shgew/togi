use test_case::test_case;

use super::parse_answer;

#[test_case("Yes \n", true ; "yes with a trailing space")]
#[test_case("Y\n", true ; "capital y")]
#[test_case("yEs\n", true ; "mixed case yes")]
#[test_case("no\n", false ; "no")]
#[test_case("sure\n", false ; "sure")]
#[test_case("\n", false ; "empty line")]
#[test_case("yes", false ; "incomplete line")]
#[test_case("", false ; "end of input")]
fn only_a_complete_line_saying_y_or_yes_is_a_yes(line: &str, want: bool) {
    assert_eq!(parse_answer(line), want);
}
