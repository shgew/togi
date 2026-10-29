use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;

use test_case::test_case;
use togi_command::{Invocation, Output};
use togi_machine::BootId;

use super::persistent_with;

fn output(stdout: &str, code: i32) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: b"query diagnostic".to_vec(),
    }
}

#[test]
fn persistence_queries_only_the_exact_boot_in_persistent_storage() {
    let boot = BootId::from("01234567-89ab-cdef-0123-456789abcdef".to_owned());
    let found = persistent_with(&boot, |invocation| {
        assert_eq!(
            invocation,
            &Invocation::new("journalctl")
                .args([
                    "--directory=/var/log/journal",
                    "--no-pager",
                    "--quiet",
                    "--lines=1",
                    "--output=json",
                    "--output-fields=_BOOT_ID",
                ])
                .arg("--boot=01234567-89ab-cdef-0123-456789abcdef")
                .output_limit(64 * 1024)
        );
        Ok(output(
            "{\"_BOOT_ID\":\"0123456789abcdef0123456789abcdef\"}\n",
            0,
        ))
    });
    assert_eq!(found.name, "persistent_journal");
    assert!(found.ok);
    assert!(
        found
            .detail
            .contains("retention and future writes are not guaranteed")
    );
}

#[test_case(""; "no persistent entry")]
#[test_case("{\"_BOOT_ID\":\"old-boot\"}"; "only another boot")]
#[test_case("not json"; "unreadable output")]
#[test_case("{}"; "missing boot identity")]
fn an_absent_or_unproven_current_boot_never_passes(stdout: &str) {
    let found = persistent_with(&BootId::from("current-boot".to_owned()), |_| {
        Ok(output(stdout, 0))
    });
    assert!(!found.ok);
    assert!(found.detail.contains("no readable entry for this boot"));
}

#[test]
fn a_failed_persistent_query_keeps_its_diagnostic() {
    let found = persistent_with(&BootId::from("current-boot".to_owned()), |_| {
        Ok(output("", 1))
    });
    assert!(!found.ok);
    assert!(found.detail.contains("query diagnostic"));
}

#[test]
fn a_timed_out_query_is_not_persistence_proof() {
    let found = persistent_with(&BootId::from("current-boot".to_owned()), |_| {
        Err(togi_command::Error::TimedOut {
            program: "journalctl".into(),
            timeout: std::time::Duration::from_secs(10),
        })
    });
    assert!(!found.ok);
    assert!(found.detail.contains("did not finish within 10s"));
}
