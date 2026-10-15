#![expect(
    clippy::disallowed_methods,
    reason = "real_process test exercises the command parser"
)]

#[test]
fn real_process_a_wrong_argument_count_is_a_usage_error() {
    for args in [&[][..], &["only-one"][..], &["a", "b", "c"][..]] {
        let ran = std::process::Command::new(env!("CARGO_BIN_EXE_togi-tool-facts"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(ran.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8(ran.stderr).unwrap().contains("Usage"));
    }
}
