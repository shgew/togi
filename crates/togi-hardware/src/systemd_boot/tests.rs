use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;

use test_case::test_case;
use togi_tuningboot::night::Entry;
use togi_tuningboot::{Bootloader as _, COUNT_VARIABLE, Environment as _, REASON_VARIABLE};

use super::entries::{Identity, LoaderEntry, entry_id};
use super::{BootIo, DEFAULT, LAST_BOOTED, SystemdBoot, decode, encode};

const SELECTED: &str = "nixos-generation-12-specialisation-togi.conf";
const OPTIONS: &str = "init=/nix/store/current-togi/init quiet togi.tuning=1";

#[derive(Clone, Debug, Eq, PartialEq)]
enum Operation {
    Persist,
    Write(String, Option<Vec<u8>>),
}

#[derive(Clone, Copy, Debug)]
enum Failure {
    IgnoreWrite,
    ReadAfterWrite,
    WriteBefore,
    WriteAfter,
    Persist,
}

struct Memory {
    variables: BTreeMap<String, Vec<u8>>,
    state: Option<Vec<u8>>,
    entries: Vec<LoaderEntry>,
    configured_default: Option<String>,
    boot_id: String,
    operations: Vec<Operation>,
    failure: Option<Failure>,
    wrote: bool,
}

#[derive(Clone)]
struct Fake(Rc<RefCell<Memory>>);

impl Fake {
    fn new(default: Option<&str>, saved: Option<&str>) -> Self {
        let mut variables = BTreeMap::new();
        if let Some(default) = default {
            variables.insert(DEFAULT.to_owned(), encode(default));
        }
        if let Some(saved) = saved {
            variables.insert(LAST_BOOTED.to_owned(), encode(saved));
        }
        Self(Rc::new(RefCell::new(Memory {
            variables,
            state: None,
            entries: vec![loader_entry(SELECTED, "kernel.efi", "initrd.efi", OPTIONS)],
            configured_default: None,
            boot_id: "first-boot".to_owned(),
            operations: Vec::new(),
            failure: None,
            wrote: false,
        })))
    }

    fn backend(&self) -> SystemdBoot {
        SystemdBoot::with_io("togi".to_owned(), Box::new(self.clone()))
    }

    fn text(&self, name: &str) -> Option<String> {
        self.0
            .borrow()
            .variables
            .get(name)
            .map(|bytes| decode(bytes).unwrap())
    }

    fn set_text(&self, name: &str, text: Option<&str>) {
        let mut memory = self.0.borrow_mut();
        if let Some(text) = text {
            memory.variables.insert(name.to_owned(), encode(text));
        } else {
            memory.variables.remove(name);
        }
    }

    fn fail(&self, failure: Failure) {
        let mut memory = self.0.borrow_mut();
        memory.failure = Some(failure);
        memory.wrote = false;
    }
}

fn injected() -> io::Error {
    io::Error::other("injected boundary failure")
}

impl BootIo for Fake {
    fn lock(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn unlock(&mut self) {}
    fn read_variable(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        let mut memory = self.0.borrow_mut();
        if memory.wrote && matches!(memory.failure, Some(Failure::ReadAfterWrite)) {
            memory.failure = None;
            return Err(injected());
        }
        Ok(memory.variables.get(name).cloned())
    }

    fn write_variable(&mut self, name: &str, value: Option<&[u8]>) -> io::Result<()> {
        let mut memory = self.0.borrow_mut();
        memory
            .operations
            .push(Operation::Write(name.to_owned(), value.map(<[u8]>::to_vec)));
        if matches!(memory.failure, Some(Failure::WriteBefore)) {
            memory.failure = None;
            return Err(injected());
        }
        if matches!(memory.failure, Some(Failure::IgnoreWrite)) {
            memory.failure = None;
            return Ok(());
        }
        memory.wrote = true;
        if let Some(value) = value {
            memory.variables.insert(name.to_owned(), value.to_vec());
        } else {
            memory.variables.remove(name);
        }
        if matches!(memory.failure, Some(Failure::WriteAfter)) {
            memory.failure = None;
            return Err(injected());
        }
        Ok(())
    }

    fn load_state(&mut self) -> io::Result<Option<Vec<u8>>> {
        Ok(self.0.borrow().state.clone())
    }

    fn store_state(&mut self, value: &[u8]) -> io::Result<()> {
        let mut memory = self.0.borrow_mut();
        memory.operations.push(Operation::Persist);
        if matches!(memory.failure, Some(Failure::Persist)) {
            memory.failure = None;
            return Err(injected());
        }
        memory.state = Some(value.to_vec());
        Ok(())
    }

    fn entries(&mut self) -> io::Result<Vec<LoaderEntry>> {
        Ok(self.0.borrow().entries.clone())
    }

    fn identity(&mut self, specialisation: &str, _running: bool) -> io::Result<Identity> {
        assert_eq!(specialisation, "togi");
        Ok(Identity {
            kernel: "kernel.efi".to_owned(),
            initrd: "initrd.efi".to_owned(),
            options: OPTIONS.split_whitespace().map(str::to_owned).collect(),
        })
    }

    fn configured_default(&mut self) -> io::Result<Option<String>> {
        Ok(self.0.borrow().configured_default.clone())
    }

    fn boot_id(&mut self) -> io::Result<String> {
        Ok(self.0.borrow().boot_id.clone())
    }
}

fn loader_entry(id: &str, kernel: &str, initrd: &str, options: &str) -> LoaderEntry {
    LoaderEntry {
        id: entry_id(id).unwrap(),
        contents: format!(
            "# fixture\ntitle NixOS\nlinux /EFI/nixos/{kernel}\ninitrd /EFI/nixos/{initrd}\noptions {options}\n"
        ),
    }
}

fn tuning() -> Entry {
    Entry::try_from("togi".to_owned()).unwrap()
}

#[test_case(r#"{"variables":{},"night":"Unrecorded"}"#, true; "object state")]
#[test_case(r#"[{},"Unrecorded"]"#, false; "positional state")]
#[test_case(r#"{"variables":{},"night":{"Selected":{"selected":"night.conf","default":null,"last_booted":null}}}"#, true; "object snapshot")]
#[test_case(r#"{"variables":{},"night":{"Selected":["night.conf",null,null]}}"#, false; "positional snapshot")]
#[test_case(r#"{"variables":{},"night":{"Ended":{"boot_id":"boot"}}}"#, true; "object ended record")]
#[test_case(r#"{"variables":{},"night":{"Ended":["boot"]}}"#, false; "positional ended record")]
#[test_case(r#"{"variables":{},"night":"Unrecorded","extra":null}"#, false; "unknown state field")]
#[test_case(r#"{"variables":{},"night":{"Selected":{"selected":"night.conf","default":null,"last_booted":null,"extra":null}}}"#, false; "unknown snapshot field")]
#[test_case(r#"{"variables":{},"night":{"Ended":{"boot_id":"boot","extra":null}}}"#, false; "unknown ended field")]
fn backend_state_requires_objects_at_every_record_boundary(json: &str, valid: bool) {
    let fake = Fake::new(Some("day.conf"), Some("day.conf"));
    fake.0.borrow_mut().state = Some(json.as_bytes().to_vec());
    let result = fake.backend().state();
    assert_eq!(result.is_ok(), valid);
    if !valid {
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
    assert!(fake.0.borrow().operations.is_empty());
}

#[test_case(None; "absent default")]
#[test_case(Some("nixos-generation-3.conf"); "explicit id")]
#[test_case(Some("nixos-generation-*.conf"); "pattern")]
#[test_case(Some("@saved"); "saved pseudo entry")]
#[test_case(Some("deleted-generation.conf"); "stale deleted id")]
fn restores_exact_previous_default_and_last_booted(previous: Option<&str>) {
    let fake = Fake::new(previous, Some("previous-last-booted.conf"));
    // Preserve attributes as well as UTF-16 data, not just a reconstructed string.
    if let Some(bytes) = fake.0.borrow_mut().variables.get_mut(LAST_BOOTED) {
        bytes[..4].copy_from_slice(&3_u32.to_le_bytes());
    }
    let original = fake.0.borrow().variables.clone();
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    assert_eq!(fake.text(DEFAULT).as_deref(), Some(SELECTED));
    assert_eq!(fake.0.borrow().operations[0], Operation::Persist);
    // Reconstruct the backend after a crash reboot; persistent Default keeps selecting tuning.
    fake.0.borrow_mut().boot_id = "crash-reboot".to_owned();
    fake.set_text(LAST_BOOTED, Some(SELECTED));
    let result = fake.backend().clear_saved_entry().unwrap();
    assert_eq!(result.before, SELECTED);
    assert_eq!(result.after, previous.unwrap_or_default());
    assert_eq!(fake.0.borrow().variables, original);
    let operations = fake.0.borrow().operations.clone();
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(fake.0.borrow().operations, operations);
}

#[test_case(None; "absent last booted")]
#[test_case(Some("deleted-last-booted.conf"); "stale last booted")]
#[test_case(Some(SELECTED); "already saved tuning last booted")]
fn restores_last_booted_exactly_and_repeat_end_is_inert(previous: Option<&str>) {
    let fake = Fake::new(Some("@saved"), previous);
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    fake.set_text(LAST_BOOTED, Some(SELECTED));
    fake.backend().clear_saved_entry().unwrap();
    let operations = fake.0.borrow().operations.clone();
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(fake.text(LAST_BOOTED).as_deref(), previous);
    assert_eq!(fake.0.borrow().operations, operations);
}

#[test_case(None; "user removed default")]
#[test_case(Some("newer-day.conf"); "user chose explicit id")]
#[test_case(Some("new-pattern-*"); "user chose pattern")]
#[test_case(Some("@saved"); "user chose saved policy")]
fn preserves_newer_user_default_and_last_booted(current: Option<&str>) {
    let fake = Fake::new(Some("old-day.conf"), Some("old-last.conf"));
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    fake.set_text(DEFAULT, current);
    fake.set_text(LAST_BOOTED, Some("user-last.conf"));
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(fake.text(DEFAULT).as_deref(), current);
    assert_eq!(fake.text(LAST_BOOTED).as_deref(), Some("user-last.conf"));
}

#[test]
fn repeated_begin_keeps_the_first_snapshot() {
    let fake = Fake::new(Some("day-*"), Some("day-last.conf"));
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    let operations = fake.0.borrow().operations.clone();
    fake.set_text(LAST_BOOTED, Some(SELECTED));
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    assert_eq!(fake.0.borrow().operations, operations);
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(fake.text(DEFAULT).as_deref(), Some("day-*"));
    assert_eq!(fake.text(LAST_BOOTED).as_deref(), Some("day-last.conf"));
}

#[test_case(None, None, Some(SELECTED), Some(SELECTED); "absent default remains absent")]
#[test_case(Some("day.conf"), None, Some(SELECTED), Some(SELECTED); "explicit default unchanged")]
#[test_case(Some("day-*"), None, Some(SELECTED), Some(SELECTED); "pattern unchanged")]
#[test_case(Some("@saved"), None, Some(SELECTED), None; "EFI saved policy clears tuning")]
#[test_case(None, Some("@saved"), Some(SELECTED), None; "loader conf saved policy clears tuning")]
#[test_case(Some("day.conf"), Some("@saved"), Some(SELECTED), Some(SELECTED); "EFI overrides loader conf")]
#[test_case(Some("@saved"), None, Some("day.conf"), Some("day.conf"); "installed saved day identity unchanged")]
#[test_case(Some("@saved"), None, Some("missing.conf"), Some("missing.conf"); "missing saved entry unchanged")]
#[test_case(Some("@saved"), None, None, None; "absent last booted unchanged")]
fn menu_night_only_clears_saved_tuning(
    default: Option<&str>,
    configured: Option<&str>,
    saved: Option<&str>,
    expected: Option<&str>,
) {
    let fake = Fake::new(default, saved);
    {
        let mut memory = fake.0.borrow_mut();
        memory.configured_default = configured.map(str::to_owned);
        memory.entries.push(loader_entry(
            "day.conf",
            "day-kernel.efi",
            "day-initrd.efi",
            "init=/nix/store/current-day/init quiet",
        ));
    }
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(fake.text(DEFAULT).as_deref(), default);
    assert_eq!(fake.text(LAST_BOOTED).as_deref(), expected);
}

#[test]
fn later_menu_boot_is_not_hidden_by_previous_end() {
    let fake = Fake::new(Some("@saved"), Some("day.conf"));
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    fake.backend().clear_saved_entry().unwrap();
    fake.0.borrow_mut().boot_id = "later-menu-boot".to_owned();
    fake.set_text(LAST_BOOTED, Some(SELECTED));
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(fake.text(LAST_BOOTED), None);
}

#[test_case("kernel.efi", "initrd.efi", OPTIONS, true; "exact current generation")]
#[test_case("other-kernel.efi", "initrd.efi", OPTIONS, false; "kernel mismatch")]
#[test_case("kernel.efi", "other-initrd.efi", OPTIONS, false; "initrd mismatch")]
#[test_case("kernel.efi", "initrd.efi", "init=/nix/store/old-togi/init quiet togi.tuning=1", false; "init identity mismatch")]
#[test_case("kernel.efi", "initrd.efi", "init=/nix/store/current-togi/init quiet", false; "options mismatch")]
fn selects_only_current_system_identity(kernel: &str, initrd: &str, options: &str, succeeds: bool) {
    let fake = Fake::new(None, None);
    fake.0.borrow_mut().entries = vec![
        loader_entry(SELECTED, kernel, initrd, options),
        loader_entry(
            "nixos-generation-999-specialisation-togi.conf",
            "other-kernel.efi",
            "other-initrd.efi",
            "init=/nix/store/newer-togi/init",
        ),
    ];
    assert_eq!(
        fake.backend().select_tuning_entry(&tuning()).is_ok(),
        succeeds
    );
    assert_eq!(fake.text(DEFAULT).as_deref(), succeeds.then_some(SELECTED));
}

#[test_case("nixos-aabbcc.conf", "nixos-aabbcc.conf"; "content addressed filename")]
#[test_case("nixos-generation-12-specialisation-togi+3-1.conf", "nixos-generation-12-specialisation-togi.conf"; "boot counting stable id")]
fn resolves_entry_id_without_generation_ranking(filename: &str, expected: &str) {
    let fake = Fake::new(None, None);
    fake.0.borrow_mut().entries = vec![loader_entry(filename, "kernel.efi", "initrd.efi", OPTIONS)];
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    assert_eq!(fake.text(DEFAULT).as_deref(), Some(expected));
}

#[test]
fn ambiguous_identity_is_refused_before_snapshot_or_efi_writes() {
    let fake = Fake::new(None, None);
    fake.0.borrow_mut().entries.push(loader_entry(
        "duplicate.conf",
        "kernel.efi",
        "initrd.efi",
        OPTIONS,
    ));
    assert!(fake.backend().select_tuning_entry(&tuning()).is_err());
    assert_eq!(fake.0.borrow().operations, Vec::<Operation>::new());
}

#[test_case(Failure::IgnoreWrite; "EFI ignored write")]
#[test_case(Failure::ReadAfterWrite; "EFI readback failed")]
#[test_case(Failure::WriteBefore; "EFI write failed before effect")]
#[test_case(Failure::WriteAfter; "EFI write failed after effect")]
fn selection_failure_keeps_snapshot_for_cleanup(failure: Failure) {
    let fake = Fake::new(Some("day.conf"), Some("day-last.conf"));
    fake.fail(failure);
    assert!(fake.backend().select_tuning_entry(&tuning()).is_err());
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(fake.text(DEFAULT).as_deref(), Some("day.conf"));
    assert_eq!(fake.text(LAST_BOOTED).as_deref(), Some("day-last.conf"));
}

#[test]
fn snapshot_publication_failure_never_changes_efi() {
    let fake = Fake::new(Some("day.conf"), None);
    fake.fail(Failure::Persist);
    assert!(fake.backend().select_tuning_entry(&tuning()).is_err());
    assert_eq!(fake.0.borrow().operations, vec![Operation::Persist]);
    assert_eq!(fake.text(DEFAULT).as_deref(), Some("day.conf"));
}

#[test_case(Failure::IgnoreWrite, true; "last booted restore ignored")]
#[test_case(Failure::ReadAfterWrite, true; "last booted restore readback failed")]
#[test_case(Failure::WriteBefore, true; "last booted restore failed before effect")]
#[test_case(Failure::WriteAfter, true; "last booted restore failed after effect")]
#[test_case(Failure::IgnoreWrite, false; "default restore ignored")]
#[test_case(Failure::ReadAfterWrite, false; "default restore readback failed")]
#[test_case(Failure::WriteBefore, false; "default restore failed before effect")]
#[test_case(Failure::WriteAfter, false; "default restore failed after effect")]
fn interrupted_clear_retries_from_durable_snapshot(failure: Failure, last_booted_changed: bool) {
    let fake = Fake::new(Some("@saved"), Some("day-last.conf"));
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    if last_booted_changed {
        fake.set_text(LAST_BOOTED, Some(SELECTED));
    }
    fake.fail(failure);
    assert!(fake.backend().clear_saved_entry().is_err());
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(fake.text(DEFAULT).as_deref(), Some("@saved"));
    assert_eq!(fake.text(LAST_BOOTED).as_deref(), Some("day-last.conf"));
}

#[test]
fn last_booted_restoration_precedes_exposing_saved_default() {
    let fake = Fake::new(Some("@saved"), Some("day-last.conf"));
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    fake.set_text(LAST_BOOTED, Some(SELECTED));
    fake.0.borrow_mut().operations.clear();
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(
        fake.0.borrow().operations,
        vec![
            Operation::Write(LAST_BOOTED.to_owned(), Some(encode("day-last.conf"))),
            Operation::Write(DEFAULT.to_owned(), Some(encode("@saved"))),
            Operation::Persist,
        ]
    );
}

#[test]
fn completed_restore_with_failed_metadata_sync_preserves_both_values() {
    let fake = Fake::new(Some("@saved"), Some("day-last.conf"));
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    fake.set_text(LAST_BOOTED, Some(SELECTED));
    fake.fail(Failure::Persist);
    assert!(fake.backend().clear_saved_entry().is_err());
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(fake.text(DEFAULT).as_deref(), Some("@saved"));
    assert_eq!(fake.text(LAST_BOOTED).as_deref(), Some("day-last.conf"));
}

#[test]
fn restart_count_and_reason_persist_independently_of_journal_and_night() {
    let fake = Fake::new(None, None);
    fake.backend()
        .set(&[(COUNT_VARIABLE, "2"), (REASON_VARIABLE, "durable reason")])
        .unwrap();
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(
        fake.backend().get(COUNT_VARIABLE).unwrap().as_deref(),
        Some("2")
    );
    assert_eq!(
        fake.backend().get(REASON_VARIABLE).unwrap().as_deref(),
        Some("durable reason")
    );
    fake.backend().unset(&[REASON_VARIABLE]).unwrap();
    assert_eq!(fake.backend().get(REASON_VARIABLE).unwrap(), None);
    assert_eq!(
        fake.backend().get(COUNT_VARIABLE).unwrap().as_deref(),
        Some("2")
    );
}

#[test_case(&[]; "missing attributes")]
#[test_case(&[7, 0, 0, 0, 65]; "odd payload length")]
#[test_case(&[7, 0, 0, 0, 65, 0]; "missing terminator")]
#[test_case(&[7, 0, 0, 0, 0, 216, 0, 0]; "invalid UTF16")]
fn invalid_efi_strings_are_refused(bytes: &[u8]) {
    assert!(decode(bytes).is_err());
}

#[test]
fn repeated_begin_after_configuration_switch_keeps_pinned_entry_and_original_day() {
    let fake = Fake::new(Some("day-*"), Some("day-last.conf"));
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    fake.0.borrow_mut().entries = vec![loader_entry(
        "nixos-new-configuration.conf",
        "kernel.efi",
        "initrd.efi",
        OPTIONS,
    )];
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    assert_eq!(fake.text(DEFAULT).as_deref(), Some(SELECTED));
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(fake.text(DEFAULT).as_deref(), Some("day-*"));
    assert_eq!(fake.text(LAST_BOOTED).as_deref(), Some("day-last.conf"));
}

#[test_case("/nix/store/kernel-linux/bzImage", "kernel-linux-bzImage.efi"; "kernel inside store directory")]
#[test_case("/nix/store/initrd-linux/initrd", "initrd-linux-initrd.efi"; "initrd inside store directory")]
#[test_case("/nix/store/initrd-linux", "initrd-linux.efi"; "store file artifact")]
fn copied_store_identity_is_cross_platform(path: &str, expected: &str) {
    assert_eq!(
        super::entries::copied_filename(std::path::Path::new(path)).unwrap(),
        expected
    );
}

#[test]
fn begin_after_external_fallback_replaces_stale_snapshot_with_current_day() {
    let fake = Fake::new(Some("old-day.conf"), Some("old-last.conf"));
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    fake.set_text(DEFAULT, None);
    fake.set_text(LAST_BOOTED, None);
    fake.backend().select_tuning_entry(&tuning()).unwrap();
    fake.backend().clear_saved_entry().unwrap();
    assert_eq!(fake.text(DEFAULT), None);
    assert_eq!(fake.text(LAST_BOOTED), None);
}
