use test_case::test_case;

use super::*;
use crate::cfg::CfgValue;
use crate::chain;

fn host() -> Host {
    Host::from_rustc(
        "rustc 1.90.0\nhost: x86_64-unknown-linux-gnu\nrelease: 1.90.0\n",
        "unix\ntarget_os=\"linux\"\ntarget_has_atomic_load_store=\"64\"\n",
    )
    .unwrap()
}

#[test]
fn the_triple_is_rustcs_host() {
    assert_eq!(host().triple, "x86_64-unknown-linux-gnu");
}

#[test_case("unix", CfgValue::True ; "printed name")]
#[test_case(r#"target_os = "macos""#, CfgValue::False ; "printed key with another value")]
#[test_case("coverage", CfgValue::True ; "cargo llvm-cov's option")]
#[test_case("coverage_nightly", CfgValue::Unknown ; "a name nothing decides")]
#[test_case("target_has_atomic_load_store", CfgValue::False ; "a printed key as a bare name")]
#[test_case(r#"target_has_atomic_load_store = "64""#, CfgValue::True ; "a printed key and value")]
fn the_cfg_is_rustcs_options_and_coverage(predicate: &str, want: CfgValue) {
    assert_eq!(host().cfg.eval(predicate), want);
}

#[test]
fn version_output_without_a_host_is_refused() {
    assert_eq!(
        chain(&Host::from_rustc("rustc 1.90.0\n", "").unwrap_err()),
        "rustc -vV names no host"
    );
}
