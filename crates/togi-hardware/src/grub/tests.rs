use std::cell::RefCell;
use std::collections::BTreeMap;
use std::error::Error as _;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::ExitStatus;
use std::rc::Rc;
use std::time::Duration;

use tempfile::TempDir;
use test_case::test_case;
use togi_tuningboot::{
    COUNT_VARIABLE, Environment as _, LeaveReason, REASON_VARIABLE, RestartCount, clear_reason,
    read_reason, reset_count, restart_limit, write_reason,
};

use super::{
    Bootloader as _, ClearError, CommandError, Edit, Editenv, Grub, GrubError, Process, SavedEntry,
    lookup,
};

const INJECTED: &str = "injected grub-editenv failure";

fn injected() -> CommandError {
    CommandError::Exit {
        status: ExitStatus::from_raw(1 << 8),
        stderr: INJECTED.to_owned(),
    }
}

fn is_injected(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if matches!(
            error.downcast_ref::<CommandError>(),
            Some(CommandError::Exit { stderr, .. }) if stderr == INJECTED
        ) {
            return true;
        }
        current = error.source();
    }
    false
}

#[derive(Default)]
struct Sim {
    saved: String,
    values: BTreeMap<String, String>,
    calls: usize,
    fail_at: usize,
    after_effect: bool,
    sticky: bool,
    operations: Vec<Edit>,
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<Sim>>);

impl Fake {
    fn with(saved: &str) -> Self {
        let fake = Self::default();
        fake.0.borrow_mut().saved = saved.to_owned();
        fake
    }

    fn failing(self, at: usize, after_effect: bool) -> Self {
        {
            let mut sim = self.0.borrow_mut();
            sim.fail_at = at;
            sim.after_effect = after_effect;
        }
        self
    }

    fn value(self, name: &str, value: &str) -> Self {
        self.0
            .borrow_mut()
            .values
            .insert(name.to_owned(), value.to_owned());
        self
    }

    fn sticky(self) -> Self {
        self.0.borrow_mut().sticky = true;
        self
    }

    fn grub(&self) -> Grub {
        Grub::with_editenv(
            Path::new("test grub environment").to_owned(),
            Box::new(self.clone()),
        )
    }

    fn calls(&self) -> usize {
        self.0.borrow().calls
    }

    fn saved(&self) -> String {
        self.0.borrow().saved.clone()
    }

    fn value_of(&self, name: &str) -> Option<String> {
        self.0.borrow().values.get(name).cloned()
    }

    fn operations(&self) -> Vec<Edit> {
        self.0.borrow().operations.clone()
    }
}

impl Editenv for Fake {
    fn run(&mut self, _env: &Path, edit: &Edit) -> Result<String, CommandError> {
        let mut sim = self.0.borrow_mut();
        sim.calls += 1;
        sim.operations.push(edit.clone());
        let fail = sim.calls == sim.fail_at;
        if fail && !sim.after_effect {
            return Err(injected());
        }
        let mut output = String::new();
        match edit {
            Edit::List => {
                output.push_str("next_entry=normal\n");
                if !sim.saved.is_empty() {
                    writeln!(output, "saved_entry={}", sim.saved).unwrap();
                }
                for (name, value) in &sim.values {
                    writeln!(output, "{name}={value}").unwrap();
                }
            }
            Edit::Unset(names) => {
                for name in names {
                    if name == "saved_entry" {
                        if !sim.sticky {
                            sim.saved.clear();
                        }
                    } else {
                        sim.values.remove(name);
                    }
                }
            }
            Edit::Set(pairs) => {
                for (name, value) in pairs {
                    sim.values.insert(name.clone(), value.clone());
                }
            }
        }
        if fail { Err(injected()) } else { Ok(output) }
    }
}

struct Listing(&'static str);

impl Editenv for Listing {
    fn run(&mut self, _env: &Path, edit: &Edit) -> Result<String, CommandError> {
        Ok(if *edit == Edit::List {
            self.0.to_owned()
        } else {
            String::new()
        })
    }
}

fn saved(before: &str) -> SavedEntry {
    SavedEntry {
        before: before.to_owned(),
        after: String::new(),
    }
}

#[test]
fn clearing_fails_in_each_of_its_three_commands_with_what_was_known() {
    for at in 1..=3 {
        for after_effect in [false, true] {
            let fake = Fake::with("togi").failing(at, after_effect);
            let error = fake.grub().clear_saved().unwrap_err();
            assert!(
                is_injected(&error),
                "operation {at} after {after_effect}: {error:?}"
            );
            let case = format!("operation {at} after {after_effect}");
            match at {
                1 => {
                    assert!(
                        matches!(error, ClearError::ReadBefore(_)),
                        "{case}: {error:?}"
                    );
                    assert_eq!(error.before(), None, "{case}");
                }
                2 => {
                    assert!(
                        matches!(error, ClearError::Unset { .. }),
                        "{case}: {error:?}"
                    );
                    assert_eq!(error.before(), Some("togi"), "{case}");
                }
                _ => {
                    assert!(
                        matches!(error, ClearError::ReadAfter { .. }),
                        "{case}: {error:?}"
                    );
                    assert_eq!(error.before(), Some("togi"), "{case}");
                }
            }
            assert_eq!(error.after(), None, "{case}");
            let unset = at == 3 || (at == 2 && after_effect);
            assert_eq!(fake.saved().is_empty(), unset, "{case}");

            let remaining = fake.saved();
            fake.0.borrow_mut().fail_at = 0;
            assert_eq!(
                fake.grub().clear_saved().unwrap(),
                saved(&remaining),
                "{case}"
            );
            assert_eq!(fake.saved(), "", "{case}");
        }
    }
}

#[test]
fn a_saved_entry_that_survives_the_unset_is_rejected_with_both_observations() {
    let fake = Fake::with("togi").sticky();
    let error = fake.grub().clear_saved().unwrap_err();
    assert!(matches!(error, ClearError::Remains { .. }), "{error:?}");
    assert_eq!(error.before(), Some("togi"));
    assert_eq!(error.after(), Some("togi"));
    assert!(
        error.to_string().contains("saved_entry is still \"togi\""),
        "{error}"
    );
    assert_eq!(fake.saved(), "togi");
}

#[test]
fn an_absent_saved_entry_clears_idempotently() {
    let fake = Fake::default();
    assert_eq!(fake.grub().clear_saved().unwrap(), saved(""));
    assert_eq!(fake.operations().len(), 3);
}

#[test]
fn the_bootloader_trait_reports_a_clear_failure_as_an_io_error_holding_the_typed_error() {
    let fake = Fake::with("togi").sticky();
    let error = fake.grub().clear_saved_entry().unwrap_err();
    let inner = error
        .get_ref()
        .unwrap()
        .downcast_ref::<ClearError>()
        .unwrap();
    assert_eq!(inner.before(), Some("togi"));
}

#[test_case("next_entry=togi\nother_saved_entry=wrong\nsaved_entry_extra=wrong\n", None; "similar names are not the variable")]
#[test_case("next_entry=normal\nsaved_entry= togi-specialisation \t\n", Some(" togi-specialisation \t"); "surrounding whitespace is kept")]
#[test_case("saved_entry=menu=entry\n", Some("menu=entry"); "only the first equals splits")]
#[test_case("saved_entry=\n", Some(""); "empty value")]
#[test_case("next_entry=normal\nsaved_entry=togi", Some("togi"); "no final newline")]
#[test_case("saved_entry=first\nsaved_entry=second\n", Some("first"); "first of two wins")]
#[test_case("saved_entry=value\r\n", Some("value\r"); "carriage return is kept")]
fn saved_entry_parsing_keeps_exactly_the_value(listing: &str, want: Option<&str>) {
    assert_eq!(
        lookup(listing, "saved_entry").as_deref(),
        want,
        "{listing:?}"
    );
}

#[test]
fn get_reads_through_the_listing_and_reports_absent_as_none() {
    let mut grub = Grub::with_editenv("env".into(), Box::new(Listing("a=1\nb=\n")));
    assert_eq!(grub.get("a").unwrap().as_deref(), Some("1"));
    assert_eq!(grub.get("b").unwrap().as_deref(), Some(""));
    assert_eq!(grub.get("missing").unwrap(), None);
}

#[test]
fn set_writes_every_pair_in_one_sorted_command_and_unset_leaves_the_saved_entry() {
    let fake = Fake::with("togi");
    let mut grub = fake.grub();
    let reason = r#"{"id":"reason","count":2,"reason":"retry"}"#;
    grub.set(&[(COUNT_VARIABLE, "2"), (REASON_VARIABLE, reason)])
        .unwrap();
    assert_eq!(
        fake.operations(),
        [Edit::Set(vec![
            (REASON_VARIABLE.to_owned(), reason.to_owned()),
            (COUNT_VARIABLE.to_owned(), "2".to_owned()),
        ])]
    );
    assert_eq!(grub.get(COUNT_VARIABLE).unwrap().as_deref(), Some("2"));
    assert_eq!(grub.get(REASON_VARIABLE).unwrap().as_deref(), Some(reason));
    assert_eq!(grub.get("missing").unwrap(), None);
    grub.unset(&[COUNT_VARIABLE, REASON_VARIABLE]).unwrap();
    assert!(fake.0.borrow().values.is_empty());
    assert_eq!(fake.saved(), "togi");
}

#[test]
fn an_empty_set_or_unset_runs_nothing() {
    let fake = Fake::default();
    let mut grub = fake.grub();
    grub.set(&[]).unwrap();
    grub.unset(&[]).unwrap();
    assert_eq!(fake.calls(), 0);
}

#[test_case("reason", "retry\nsaved_entry=togi"; "value with a newline")]
#[test_case("reason", "retry\0"; "value with a nul")]
#[test_case("reason", "retry ☃"; "value that is not ascii")]
#[test_case("reason", &"a".repeat(301); "value of 301 bytes")]
#[test_case("", "retry"; "empty name")]
#[test_case("reason=saved_entry", "retry"; "name with an equals sign")]
#[test_case("reason\n", "retry"; "name with a newline")]
#[test_case(&"a".repeat(65), "retry"; "name of 65 bytes")]
fn invalid_names_and_values_are_refused_before_any_command(name: &str, value: &str) {
    let fake = Fake::default();
    let mut grub = fake.grub();
    let error = grub.set(&[(name, value)]).unwrap_err();
    assert_eq!(
        error.kind(),
        io::ErrorKind::InvalidInput,
        "{name:?} {value:?}"
    );
    assert_eq!(fake.calls(), 0, "set {name:?} {value:?}");
    if name != "reason" {
        assert!(grub.get(name).is_err());
        assert!(grub.unset(&["saved_entry", name]).is_err());
        assert_eq!(fake.calls(), 0, "get/unset {name:?}");
    }
}

#[test]
fn a_value_of_exactly_300_printable_bytes_is_accepted() {
    let fake = Fake::default();
    let value = "a".repeat(300);
    fake.grub().set(&[("reason", &value)]).unwrap();
    assert_eq!(fake.value_of("reason").as_deref(), Some(value.as_str()));
}

#[test_case(" "; "a space")]
#[test_case(" 1"; "leading space")]
#[test_case("1 "; "trailing space")]
#[test_case("\t2"; "leading tab")]
#[test_case("3\r"; "trailing carriage return")]
fn whitespace_counts_are_read_exactly_and_grant_no_retry(value: &str) {
    let fake = Fake::with("togi").value(COUNT_VARIABLE, value);
    let mut grub = fake.grub();
    assert_eq!(grub.get(COUNT_VARIABLE).unwrap().as_deref(), Some(value));
    let error = restart_limit(&mut grub).unwrap_err();
    assert!(
        matches!(error, togi_tuningboot::Error::InvalidCount(_)),
        "{error:?}"
    );
    assert_eq!(fake.saved(), "togi");
    assert_eq!(fake.value_of(COUNT_VARIABLE).as_deref(), Some(value));
    assert_eq!(fake.0.borrow().values.len(), 1);
    assert_eq!(fake.operations(), [Edit::List, Edit::List], "{value:?}");
}

#[test]
fn exhausting_the_restart_limit_fails_in_each_command_without_granting_a_retry() {
    for at in 1..=5 {
        for after_effect in [false, true] {
            let fake = Fake::with("togi")
                .value(COUNT_VARIABLE, "2")
                .failing(at, after_effect);
            let error = restart_limit(&mut fake.grub()).unwrap_err();
            assert!(
                is_injected(&error),
                "operation {at} after {after_effect}: {error:?}"
            );
            let written = at > 2 || (at == 2 && after_effect);
            assert_eq!(
                fake.value_of(REASON_VARIABLE).is_some(),
                written,
                "{at} {after_effect}"
            );
            let cleared = at > 4 || (at == 4 && after_effect);
            assert_eq!(fake.saved().is_empty(), cleared, "{at} {after_effect}");
        }
    }
}

#[test]
fn reason_reads_writes_and_clears_keep_the_cause_whether_or_not_the_effect_happened() {
    for after_effect in [false, true] {
        let fake = Fake::default().failing(1, after_effect);
        let error = read_reason(&mut fake.grub()).unwrap_err();
        assert!(is_injected(&error), "read after {after_effect}: {error:?}");

        let fake = Fake::default().failing(1, after_effect);
        let count = RestartCount::try_from(1).unwrap();
        let reason = LeaveReason::new("retry", count).unwrap();
        let error = write_reason(&mut fake.grub(), &reason).unwrap_err();
        assert!(is_injected(&error), "write after {after_effect}: {error:?}");

        let fake = Fake::default()
            .value(REASON_VARIABLE, "record")
            .failing(1, after_effect);
        let error = clear_reason(&mut fake.grub()).unwrap_err();
        assert!(is_injected(&error), "clear after {after_effect}: {error:?}");

        let fake = Fake::default()
            .value(COUNT_VARIABLE, "2")
            .failing(1, after_effect);
        let error = reset_count(&mut fake.grub()).unwrap_err();
        assert!(is_injected(&error), "reset after {after_effect}: {error:?}");
    }
}

#[test]
fn edits_become_the_grub_editenv_verbs() {
    let args = |edit: Edit| -> Vec<String> {
        edit.args()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    };
    assert_eq!(args(Edit::List), ["list"]);
    assert_eq!(
        args(Edit::Set(vec![
            ("a".into(), "1 2".into()),
            ("b".into(), String::new())
        ])),
        ["set", "a=1 2", "b="]
    );
    assert_eq!(
        args(Edit::Unset(vec!["a".into(), "b".into()])),
        ["unset", "a", "b"]
    );
}

fn script(body: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("editenv.sh");
    std::fs::write(&path, body).unwrap();
    (dir, path)
}

fn shell(deadline: Duration, output_limit: usize) -> Process {
    Process {
        program: OsString::from("/bin/sh"),
        deadline,
        output_limit,
    }
}

#[test]
fn real_process_runner_returns_standard_output_of_a_successful_command() {
    let (_dir, path) = script("echo \"$1\"; echo saved_entry=togi\n");
    let output = shell(Duration::from_secs(10), 1024)
        .run(&path, &Edit::List)
        .unwrap();
    assert_eq!(output, "list\nsaved_entry=togi\n");
}

#[test]
fn real_process_runner_reports_a_failing_exit_with_its_trimmed_stderr() {
    let (_dir, path) = script("echo ' unwritable ' >&2; exit 3\n");
    let error = shell(Duration::from_secs(10), 1024)
        .run(&path, &Edit::Unset(vec!["saved_entry".to_owned()]))
        .unwrap_err();
    assert!(
        matches!(&error, CommandError::Exit { stderr, .. } if stderr == "unwritable"),
        "{error:?}"
    );
    assert!(error.to_string().ends_with(": unwritable"), "{error}");
}

#[test]
fn real_process_runner_kills_a_hung_command_at_its_deadline() {
    let (_dir, path) = script("sleep 30\n");
    let begun = std::time::Instant::now();
    let error = shell(Duration::from_millis(300), 1024)
        .run(&path, &Edit::List)
        .unwrap_err();
    assert!(
        matches!(
            error,
            CommandError::Command(togi_command::Error::TimedOut { .. })
        ),
        "{error:?}"
    );
    assert!(begun.elapsed() < Duration::from_secs(20));
    let grub_error = GrubError::Editenv {
        env: path,
        verb: super::Verb::List,
        source: error,
    };
    assert_eq!(io::Error::from(grub_error).kind(), io::ErrorKind::TimedOut);
}

#[test]
fn real_process_runner_refuses_output_beyond_its_cap() {
    let (_dir, path) = script("head -c 4096 /dev/zero\n");
    let error = shell(Duration::from_secs(10), 1024)
        .run(&path, &Edit::List)
        .unwrap_err();
    assert!(
        matches!(
            error,
            CommandError::Command(togi_command::Error::OutputTooLarge { .. })
        ),
        "{error:?}"
    );
}

#[test]
fn real_process_runner_refuses_output_that_is_not_utf8() {
    let (_dir, path) = script("printf '\\377\\376'\n");
    let error = shell(Duration::from_secs(10), 1024)
        .run(&path, &Edit::List)
        .unwrap_err();
    assert!(matches!(error, CommandError::NotUtf8(_)), "{error:?}");
}

#[test]
fn a_grub_error_names_the_environment_and_verb_and_keeps_its_cause() {
    let fake = Fake::default().failing(1, false);
    let error = fake.grub().get("saved_entry").unwrap_err();
    let grub = error
        .get_ref()
        .unwrap()
        .downcast_ref::<GrubError>()
        .unwrap();
    assert_eq!(grub.to_string(), "grub-editenv test grub environment list");
    assert!(is_injected(grub.source().unwrap()));
}
