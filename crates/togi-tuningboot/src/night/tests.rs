use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;

use test_case::test_case;

use super::{
    BootSelection, DAY_ENTRY_VARIABLE, Entry, NIGHT_BEGUN, NIGHT_VARIABLE, NightError,
    SAVED_ENTRY_VARIABLE, Selection, Setup,
};
use crate::{Bootloader, Environment, SavedEntry};

type Values = Rc<RefCell<BTreeMap<String, String>>>;

/// GRUB's environment block in memory, shared with the test.
struct Memory(Values);

impl Environment for Memory {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        Ok(self.0.borrow().get(name).cloned())
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        for (name, value) in values {
            self.0
                .borrow_mut()
                .insert((*name).to_owned(), (*value).to_owned());
        }
        Ok(())
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        for name in names {
            self.0.borrow_mut().remove(*name);
        }
        Ok(())
    }
}

/// What the real GRUB implementation does, which `togi-hardware` tests against `grub-editenv`: a
/// begun night selects the day's remembered entry again and ends; a remembered entry already
/// selected stays; otherwise `saved_entry` and any remembered entry are unset.
impl Bootloader for Memory {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        let mut values = self.0.borrow_mut();
        let before = values
            .get(SAVED_ENTRY_VARIABLE)
            .cloned()
            .unwrap_or_default();
        let day = values
            .get(DAY_ENTRY_VARIABLE)
            .filter(|day| !day.is_empty())
            .cloned();
        let begun = values.contains_key(NIGHT_VARIABLE);
        let after = match (day, begun) {
            (Some(day), true) => {
                values.insert(SAVED_ENTRY_VARIABLE.to_owned(), day.clone());
                values.remove(NIGHT_VARIABLE);
                day
            }
            (Some(day), false) if day == before => day,
            (Some(_) | None, _) => {
                for name in [SAVED_ENTRY_VARIABLE, NIGHT_VARIABLE, DAY_ENTRY_VARIABLE] {
                    values.remove(name);
                }
                String::new()
            }
        };
        Ok(SavedEntry { before, after })
    }
}

/// A GRUB that misbehaves the way `grub-editenv` can: it keeps ignoring writes of the selection, or
/// cannot read the environment at all.
enum Fault {
    IgnoresSelectionWrites,
    Unreadable,
}

struct Flaky(Memory, Fault);

impl Environment for Flaky {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        match self.1 {
            Fault::Unreadable => Err(io::Error::other("grubenv unreadable")),
            Fault::IgnoresSelectionWrites => self.0.get(name),
        }
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        match self.1 {
            Fault::IgnoresSelectionWrites => {
                let kept: Vec<_> = values
                    .iter()
                    .copied()
                    .filter(|(name, _)| *name != SAVED_ENTRY_VARIABLE)
                    .collect();
                self.0.set(&kept)
            }
            Fault::Unreadable => self.0.set(values),
        }
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        self.0.unset(names)
    }
}

impl Bootloader for Flaky {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        self.0.clear_saved_entry()
    }
}

fn flaky(saved: Option<&str>, fault: Fault) -> (BootSelection, Values) {
    let (_, values) = selection(saved);
    (
        BootSelection::new(Box::new(Flaky(Memory(Rc::clone(&values)), fault))),
        values,
    )
}

const TUNING: &str = "NixOS - togi";
const DAY: &str = "NixOS - Configuration 7";

fn selection(saved: Option<&str>) -> (BootSelection, Values) {
    let values = Values::default();
    if let Some(saved) = saved {
        values
            .borrow_mut()
            .insert(SAVED_ENTRY_VARIABLE.to_owned(), saved.to_owned());
    }
    (
        BootSelection::new(Box::new(Memory(Rc::clone(&values)))),
        values,
    )
}

fn saved(values: &Values) -> Option<String> {
    values.borrow().get(SAVED_ENTRY_VARIABLE).cloned()
}

fn value(values: &Values, name: &str) -> Option<String> {
    values.borrow().get(name).cloned()
}

fn tuning() -> Entry {
    Entry::try_from(TUNING.to_owned()).unwrap()
}

#[test]
fn beginning_a_night_selects_the_tuning_entry_and_remembers_the_day_exactly() {
    let (mut grub, values) = selection(Some(DAY));
    grub.begin(&tuning()).unwrap();
    assert_eq!(saved(&values).as_deref(), Some(TUNING));
    assert_eq!(value(&values, DAY_ENTRY_VARIABLE).as_deref(), Some(DAY));
    assert_eq!(value(&values, NIGHT_VARIABLE).as_deref(), Some(NIGHT_BEGUN));
    assert_eq!(
        grub.end().unwrap(),
        SavedEntry {
            before: TUNING.to_owned(),
            after: DAY.to_owned()
        }
    );
    assert_eq!(saved(&values).as_deref(), Some(DAY));
    assert_eq!(value(&values, NIGHT_VARIABLE), None);
}

#[test]
fn a_day_without_a_saved_selection_returns_to_none_and_never_pins_a_generation() {
    let (mut grub, values) = selection(None);
    grub.begin(&tuning()).unwrap();
    assert_eq!(saved(&values).as_deref(), Some(TUNING));
    assert_eq!(value(&values, DAY_ENTRY_VARIABLE), None);
    grub.end().unwrap();
    assert_eq!(saved(&values), None);
    assert_eq!(value(&values, NIGHT_VARIABLE), None);
}

#[test]
fn a_day_without_a_saved_selection_forgets_the_day_of_an_earlier_night() {
    let (mut grub, values) = selection(None);
    values
        .borrow_mut()
        .insert(DAY_ENTRY_VARIABLE.to_owned(), DAY.to_owned());
    grub.begin(&tuning()).unwrap();
    assert_eq!(value(&values, DAY_ENTRY_VARIABLE), None);
    grub.end().unwrap();
    assert_eq!(saved(&values), None);
}

#[test]
fn returning_twice_selects_the_same_day_entry_again() {
    let (mut grub, values) = selection(Some(DAY));
    grub.begin(&tuning()).unwrap();
    grub.end().unwrap();
    assert_eq!(
        grub.end().unwrap(),
        SavedEntry {
            before: DAY.to_owned(),
            after: DAY.to_owned()
        }
    );
    assert_eq!(saved(&values).as_deref(), Some(DAY));
}

#[test]
fn beginning_again_in_the_night_keeps_the_day_the_first_beginning_remembered() {
    let (mut grub, values) = selection(Some(DAY));
    grub.begin(&tuning()).unwrap();
    grub.begin(&tuning()).unwrap();
    assert_eq!(value(&values, DAY_ENTRY_VARIABLE).as_deref(), Some(DAY));
    grub.end().unwrap();
    assert_eq!(saved(&values).as_deref(), Some(DAY));
}

#[test]
fn a_tuning_boot_entered_from_the_menu_is_adopted_as_a_night() {
    // GRUB's savedefault selects the entry picked in the menu; crash reboots then boot it again.
    let (mut grub, values) = selection(Some(TUNING));
    assert_eq!(saved(&values).as_deref(), Some(TUNING));
    assert_eq!(grub.end().unwrap().after, "");
    assert_eq!(saved(&values), None);
}

#[test]
fn a_stale_day_entry_is_dropped_when_a_menu_entered_night_ends() {
    let (mut grub, values) = selection(Some(DAY));
    grub.begin(&tuning()).unwrap();
    grub.end().unwrap();
    // A later tuning boot picked from the menu: savedefault selects it, and nothing begins a night.
    values
        .borrow_mut()
        .insert(SAVED_ENTRY_VARIABLE.to_owned(), TUNING.to_owned());
    grub.end().unwrap();
    assert_eq!(saved(&values), None);
    assert_eq!(value(&values, DAY_ENTRY_VARIABLE), None);
}

#[test]
fn beginning_refuses_when_the_selection_does_not_read_back_as_the_tuning_entry() {
    let (mut grub, values) = flaky(Some(DAY), Fault::IgnoresSelectionWrites);
    let error = grub.begin(&tuning()).unwrap_err();
    assert!(
        matches!(&error, NightError::NotSelected { expected, found }
            if expected == TUNING && found.as_deref() == Some(DAY)),
        "{error:?}"
    );
    assert_eq!(saved(&values).as_deref(), Some(DAY));
}

#[test]
fn beginning_on_an_unreadable_environment_fails_and_changes_nothing() {
    let (mut grub, values) = flaky(Some(DAY), Fault::Unreadable);
    let before = values.borrow().clone();
    assert!(matches!(
        grub.begin(&tuning()),
        Err(NightError::ReadSelection(_))
    ));
    assert_eq!(*values.borrow(), before);
}

#[test]
fn the_setup_names_the_environment_and_the_entry() {
    let setup: Setup = r#"{"loader":"grub","grubenv":"/boot/grub/grubenv","entry":"NixOS - togi"}"#
        .parse()
        .unwrap();
    assert_eq!(setup.boot_path().to_str(), Some("/boot/grub/grubenv"));
    assert_eq!(setup.entry().as_str(), TUNING);
}

#[test]
fn the_systemd_setup_names_the_esp_state_and_specialisation() {
    let setup: Setup = r#"{"loader":"systemd-boot","esp":"/boot","boot":"/boot","state":"/var/lib/togi-boot","specialisation":"togi"}"#
        .parse()
        .unwrap();
    assert_eq!(setup.boot_path(), std::path::Path::new(super::SETUP_PATH));
    assert_eq!(setup.entry().as_str(), "togi");
    assert!(matches!(setup, Setup::SystemdBoot { esp, state, .. }
        if esp == std::path::Path::new("/boot") && state == std::path::Path::new("/var/lib/togi-boot")));
}

#[test_case(r#"{"loader":"grub","grubenv":"/g","entry":""}"#; "empty entry")]
#[test_case(r#"{"loader":"grub","grubenv":"/g","entry":"a","extra":1}"#; "unknown field")]
#[test_case(r#"{"loader":"grub","grubenv":"/g","entry":"a","morning":"06:30"}"#; "morning time of an earlier design")]
#[test_case(r#"{"loader":"grub","entry":"a"}"#; "missing grubenv")]
#[test_case(r#"{"loader":"unsupported","entry":"a"}"#; "unknown loader")]
#[test_case(r#"["/boot/grub/grubenv","NixOS - togi"]"#; "array of the fields")]
fn a_malformed_setup_is_refused(text: &str) {
    assert!(text.parse::<Setup>().is_err(), "{text}");
}
