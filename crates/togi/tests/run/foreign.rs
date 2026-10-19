//! `togi run` over the state directory of the Go build, against fakes for the host: the first line of its journal is
//! not a togi journal header, so the run exits 1 after the platform check and before the configuration, the host
//! lock, the journal lock, the GRUB environment, the boot id and the machine, and changes no file. The Go build's
//! files are described in `fixture`.

use togi_cli::Exit;

use crate::fixture::{self, Snapshot};
use crate::support::{Fake, Grub};

/// A host whose state directory holds the Go build's files, a configuration that would be a usage error if it were
/// read, and what the host directory held.
fn go_host() -> (Fake, std::path::PathBuf, Snapshot) {
    let fake = Fake::new();
    fixture::install(&*fake.fs, &fake.state_dir());
    let bad = fake.dir.join("invalid.json");
    fake.write(&bad, "{ not a configuration").unwrap();
    let before = fixture::snapshot(&*fake.fs, &fake.dir);
    (fake, bad, before)
}

/// Nothing past the platform check ran, and nothing was created or changed under the host's directory, which holds
/// the host lock, the state directory and its lock.
fn assert_stopped_after_the_platform_check(fake: &Fake, before: &Snapshot) {
    assert_eq!(*fake.calls.borrow(), ["check_platform"]);
    assert_eq!((fake.boots_read.get(), fake.opened.get()), (0, 0));
    assert_eq!(fake.reboots.get(), 0);
    assert!(!fake.exists(&fake.lock), "the host lock was taken");
    assert!(
        !fake.exists(fake.state_dir().join("lock")),
        "the journal lock was taken"
    );
    assert_eq!(&fixture::snapshot(&*fake.fs, &fake.dir), before);
}

#[test]
fn run_refuses_the_go_journal_before_the_configuration_and_changes_no_file() {
    let (fake, bad, before) = go_host();
    let (exit, out, err) = fake.run(&["run", "--no-tui", "--config", bad.to_str().unwrap()]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert_eq!(exit.code(), 1);
    assert_eq!(out, "");
    fixture::assert_refusal("run", &err, &fake.state_dir());
    assert!(!err.contains("configuration"), "{err}");
    assert_stopped_after_the_platform_check(&fake, &before);
}

#[test]
fn the_tuning_boot_refuses_the_go_journal_without_a_bootloader_host_or_machine_operation() {
    let (mut fake, bad, before) = go_host();
    fake.grub = Grub::with_saved_entry("tuning");
    let grubenv = fake.grubenv();
    let (exit, out, err) = fake.run(&[
        "run",
        "--tuning-boot",
        grubenv.to_str().unwrap(),
        "--config",
        bad.to_str().unwrap(),
    ]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert_eq!(out, "");
    fixture::assert_refusal("run", &err, &fake.state_dir());
    assert!(!err.contains("cleared GRUB"), "{err}");
    let grub = fake.grub.state();
    assert_eq!(grub.cleared, 0);
    assert!(
        grub.log.is_empty(),
        "no leave reason, no clear: {:?}",
        grub.log
    );
    assert_eq!(
        grub.values.keys().collect::<Vec<_>>(),
        ["saved_entry"],
        "{:?}",
        grub.values
    );
    drop(grub);
    assert!(!fake.exists(grubenv));
    assert_stopped_after_the_platform_check(&fake, &before);
}

#[test]
fn the_refusal_comes_from_the_first_line_alone_whatever_follows_it() {
    let (fake, bad, _) = go_host();
    fixture::append_unreadable_line(&*fake.fs, &fake.state_dir());
    let before = fixture::snapshot(&*fake.fs, &fake.dir);
    let (exit, _, err) = fake.run(&["run", "--no-tui", "--config", bad.to_str().unwrap()]);
    assert_eq!(exit, Exit::Error, "{err}");
    fixture::assert_refusal("run", &err, &fake.state_dir());
    assert!(!err.contains("unknown event kind"), "{err}");
    assert_stopped_after_the_platform_check(&fake, &before);
}

#[test]
fn a_first_line_without_its_newline_is_refused_all_the_same() {
    let (fake, bad, _) = go_host();
    fixture::keep_only_the_unterminated_first_line(&*fake.fs, &fake.state_dir());
    let before = fixture::snapshot(&*fake.fs, &fake.dir);
    let (exit, out, err) = fake.run(&["run", "--no-tui", "--config", bad.to_str().unwrap()]);
    assert_eq!(exit, Exit::Error, "{err}");
    assert_eq!(out, "");
    fixture::assert_refusal("run", &err, &fake.state_dir());
    assert!(!err.contains("torn bytes"), "{err}");
    assert_stopped_after_the_platform_check(&fake, &before);
}

#[test_case::test_case(false; "unterminated")]
#[test_case::test_case(true; "terminated")]
fn a_truncated_header_of_this_build_is_refused_in_the_same_words(newline: bool) {
    let fake = Fake::new();
    let bad = fake.dir.join("invalid.json");
    fake.write(&bad, "{ not a configuration").unwrap();
    fake.create_dir(fake.state_dir()).unwrap();
    fixture::keep_only_a_truncated_journal_header(&*fake.fs, &fake.state_dir(), newline);
    let before = fixture::snapshot(&*fake.fs, &fake.dir);
    let (exit, out, err) = fake.run(&["run", "--no-tui", "--config", bad.to_str().unwrap()]);
    assert_eq!(exit, Exit::Error, "newline {newline}: {err}");
    assert_eq!(out, "");
    fixture::assert_refusal("run", &err, &fake.state_dir());
    assert!(!err.contains("torn bytes"), "{err}");
    assert_stopped_after_the_platform_check(&fake, &before);
}

#[test]
fn an_unsupported_platform_is_still_reported_first() {
    let (mut fake, bad, before) = go_host();
    fake.supported = false;
    let (exit, _, err) = fake.run(&["run", "--no-tui", "--config", bad.to_str().unwrap()]);
    assert_eq!(exit, Exit::Error);
    assert_eq!(
        err,
        "togi run: hardware runs need Linux: unsupported operation\n"
    );
    assert_stopped_after_the_platform_check(&fake, &before);
}
