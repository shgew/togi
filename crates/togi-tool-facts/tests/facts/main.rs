#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "real_process test exercises the command parser and panics on a broken fixture"
)]

use test_case::test_case;

#[test_case(&[]; "no arguments")]
#[test_case(&["only-one"]; "one argument")]
#[test_case(&["a", "b", "c"]; "three arguments")]
fn real_process_a_wrong_argument_count_is_a_usage_error(args: &[&str]) {
    let ran = std::process::Command::new(env!("CARGO_BIN_EXE_togi-tool-facts"))
        .args(args)
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(2), "{args:?}");
    assert!(String::from_utf8(ran.stderr).unwrap().contains("Usage"));
}
