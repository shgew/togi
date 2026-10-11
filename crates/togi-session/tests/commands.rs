#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

use std::io;

use jiff::tz::TimeZone;
use togi_journal::fs::{Fault, Op};
use togi_journal::{Build, Journal, MemFs, Payload, ResetTarget, SCHEMA, StateDir};
use togi_machine::{CancellationToken, CoreId};
use togi_render::Renderer;
use togi_session::{
    CommandError, ConfigSource, PublicationPolicy, RunInput, StopPolicy, reset_all, reset_core, run,
};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};

fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: 1,
        schema: SCHEMA,
        fixes: 0,
        evidence: 1,
    }
}

fn core(n: i64) -> CoreId {
    CoreId::new(n).unwrap()
}

#[expect(
    clippy::disallowed_methods,
    reason = "the tests keep the journal in memory"
)]
fn dir(mem: &MemFs) -> StateDir {
    StateDir::simulated(mem.clone()).unwrap()
}

fn simulator() -> Simulator {
    let definition = MachineDefinition {
        cores: 2,
        ..MachineDefinition::default()
    };
    Simulator::new(
        Model::new(&definition, 1).unwrap(),
        RunSettings {
            seed: 1,
            ..RunSettings::default()
        },
    )
}

fn input(sim: &Simulator, mem: &MemFs) -> RunInput {
    let mut machine = sim.seams();
    let boot = machine.host.boot_id().unwrap();
    let journal = dir(mem).open_at(boot.clone(), sim.now()).unwrap();
    RunInput {
        config: togi_config::Config::default(),
        source: ConfigSource::default(),
        build: build(),
        boot,
        journal,
        machine,
        policy: StopPolicy::default(),
        publication: PublicationPolicy::OnFinish,
        bootloader: None,
        prompt: None,
        defects: &[],
        resume: None,
        carry: None,
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        report: Box::new(io::sink()),
    }
}

fn started(sim: &Simulator, mem: &MemFs) -> Journal {
    let input = input(sim, mem);
    input.cancel.cancel();
    let ended = run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    ended.journal
}

fn kinds(journal: &Journal) -> Vec<&'static str> {
    journal
        .events()
        .iter()
        .map(|event| event.kind().as_str())
        .collect()
}

#[test]
fn reset_core_appends_the_command_and_nothing_else() {
    let (sim, mem) = (simulator(), MemFs::new());
    let mut journal = started(&sim, &mem);
    let before = kinds(&journal);
    let seq = reset_core(&build(), &mut journal, &mut *sim.seams().clock, core(1)).unwrap();
    let after = kinds(&journal);
    assert_eq!(after[..before.len()], before[..]);
    assert_eq!(after[before.len()..], ["command.reset"]);
    let event = journal.event(seq).unwrap();
    assert!(matches!(
        &event.payload,
        Payload::CommandReset(reset) if reset.target == ResetTarget::Core(core(1))
    ));
}

#[test]
fn reset_core_refuses_a_core_the_session_lacks_and_leaves_the_journal_alone() {
    let (sim, mem) = (simulator(), MemFs::new());
    let mut journal = started(&sim, &mem);
    let before = kinds(&journal);
    let error = reset_core(&build(), &mut journal, &mut *sim.seams().clock, core(9)).unwrap_err();
    assert!(matches!(error, CommandError::NoSuchCore { core: lacking } if lacking == core(9)));
    assert_eq!(kinds(&journal), before);
}

#[test]
fn reset_core_never_ends_the_session_it_resets() {
    let (sim, mem) = (simulator(), MemFs::new());
    let mut journal = started(&sim, &mem);
    reset_core(&build(), &mut journal, &mut *sim.seams().clock, core(0)).unwrap();
    drop(journal);
    assert!(dir(&mem).archives().unwrap().is_empty());
}

#[test]
fn reset_all_archives_the_session_and_the_directory_starts_empty() {
    let (sim, mem) = (simulator(), MemFs::new());
    let journal = started(&sim, &mem);
    let (session, dir) = reset_all(&build(), journal, &mut *sim.seams().clock).unwrap();
    assert_eq!(dir.archives().unwrap(), [session]);
    drop(dir);
    let boot = sim.seams().host.boot_id().unwrap();
    let reopened = self::dir(&mem).open_at(boot, sim.now()).unwrap();
    assert!(reopened.events().is_empty());
}

#[test]
fn reset_all_archives_a_session_of_another_schema_unread() {
    let (sim, mem) = (simulator(), MemFs::new());
    let journal = started(&sim, &mem);
    let mut other = build();
    other.schema += 1;
    let (session, dir) = reset_all(&other, journal, &mut *sim.seams().clock).unwrap();
    assert_eq!(dir.archives().unwrap(), [session]);
}

#[test]
fn reset_all_that_cannot_archive_fails_and_releases_the_lock() {
    let (sim, mem) = (simulator(), MemFs::new());
    let journal = started(&sim, &mem);
    mem.inject(Fault::before(Op::Rename, 1));
    let error = reset_all(&build(), journal, &mut *sim.seams().clock).unwrap_err();
    assert!(matches!(error, CommandError::Archive(_)), "{error:?}");
    drop(dir(&mem));
}
