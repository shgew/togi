#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

use std::io;
use std::num::NonZeroU32;

use jiff::tz::TimeZone;
use togi_journal::fs::{Fault, Op};
use togi_journal::{
    Build, CorePhase, CycleEvent, FixStamp, Journal, MemFs, Payload, ResetTarget, SCHEMA, StateDir,
};
use togi_machine::{CancellationToken, CoreId};
use togi_render::Renderer;
use togi_session::{
    CommandError, ConfigSource, PublicationPolicy, RunInput, Stop, StopPolicy, reset_all,
    reset_core, run,
};
use togi_sim::definition::ModelDef;
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};

fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: 1,
        schema: SCHEMA,
        fixes: FixStamp::none(),
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
    simulator_of(&MachineDefinition {
        cores: 2,
        ..MachineDefinition::default()
    })
}

fn simulator_of(definition: &MachineDefinition) -> Simulator {
    Simulator::new(
        Model::new(definition, 1).unwrap(),
        RunSettings {
            seed: 1,
            ..RunSettings::default()
        },
    )
}

fn input(sim: &Simulator, mem: &MemFs) -> RunInput<'static> {
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
        on_append: None,
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

#[test]
fn a_failed_reset_core_append_keeps_the_durable_prefix_and_only_a_synced_command_survives() {
    for (fault, survives) in [(Op::Write, false), (Op::Sync, true)] {
        let (sim, mem) = (simulator(), MemFs::new());
        let mut journal = started(&sim, &mem);
        let before = kinds(&journal);
        mem.inject(Fault::before(fault, 1).named("events.jsonl"));
        let error =
            reset_core(&build(), &mut journal, &mut *sim.seams().clock, core(1)).unwrap_err();
        assert!(
            matches!(error, CommandError::Append(_)),
            "{fault:?}: {error:?}"
        );
        drop(journal);
        mem.clear_faults();
        let boot = sim.seams().host.boot_id().unwrap();
        let reopened = dir(&mem).open_at(boot, sim.now()).unwrap();
        let after = kinds(&reopened);
        assert_eq!(after[..before.len()], before[..], "{fault:?}");
        if survives {
            assert_eq!(after[before.len()..], ["command.reset"], "{fault:?}");
            let event = reopened.events().last().unwrap();
            assert!(matches!(
                &event.payload,
                Payload::CommandReset(reset) if reset.target == ResetTarget::Core(core(1))
            ));
        } else {
            assert_eq!(after, before, "{fault:?}");
        }
    }
}

#[test]
fn a_failed_reset_all_append_returns_an_append_error_releases_the_lock_and_archives_nothing() {
    for (fault, survives) in [(Op::Write, false), (Op::Sync, true)] {
        let (sim, mem) = (simulator(), MemFs::new());
        let journal = started(&sim, &mem);
        let before = kinds(&journal);
        mem.inject(Fault::before(fault, 1).named("events.jsonl"));
        let error = reset_all(&build(), journal, &mut *sim.seams().clock).unwrap_err();
        assert!(
            matches!(error, CommandError::Append(_)),
            "{fault:?}: {error:?}"
        );
        mem.clear_faults();
        let boot = sim.seams().host.boot_id().unwrap();
        let state = dir(&mem);
        assert!(state.archives().unwrap().is_empty(), "{fault:?}");
        let reopened = state.open_at(boot, sim.now()).unwrap();
        let after = kinds(&reopened);
        assert_eq!(after[..before.len()], before[..], "{fault:?}");
        if survives {
            assert_eq!(after[before.len()..], ["command.reset"], "{fault:?}");
            let event = reopened.events().last().unwrap();
            assert!(matches!(
                &event.payload,
                Payload::CommandReset(reset) if reset.target == ResetTarget::All
            ));
        } else {
            assert_eq!(after, before, "{fault:?}");
        }
    }
}

#[test]
fn reset_all_of_an_empty_journal_is_no_session_and_releases_the_lock() {
    let (sim, mem) = (simulator(), MemFs::new());
    let boot = sim.seams().host.boot_id().unwrap();
    let journal = dir(&mem).open_at(boot.clone(), sim.now()).unwrap();
    assert!(journal.events().is_empty());
    let error = reset_all(&build(), journal, &mut *sim.seams().clock).unwrap_err();
    assert!(matches!(error, CommandError::NoSession), "{error:?}");
    let state = dir(&mem);
    assert!(state.archives().unwrap().is_empty());
    assert!(state.open_at(boot, sim.now()).unwrap().events().is_empty());
}

fn phases_of(journal: &Journal, core: CoreId) -> Vec<(Option<CorePhase>, CorePhase, i64)> {
    journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::CorePhase(phase) = &event.payload
                && phase.core == core
            {
                Some((phase.from, phase.to, phase.offset.get()))
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn a_reset_core_is_consumed_by_the_resumed_session_which_retunes_it_to_its_limit() {
    let steady = MachineDefinition {
        cores: 2,
        model: ModelDef {
            past_limit_rate: 0.0,
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    };
    let (sim, mem) = (simulator_of(&steady), MemFs::new());
    let mut first = input(&sim, &mem);
    first.policy.cycles = Some(NonZeroU32::MIN);
    let ended = run(first);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let mut journal = ended.journal;
    let limit = phases_of(&journal, core(1))
        .into_iter()
        .rev()
        .find(|(_, to, _)| *to == CorePhase::AtLimit)
        .map(|(_, _, offset)| offset)
        .unwrap();
    let reset = reset_core(&build(), &mut journal, &mut *sim.seams().clock, core(1)).unwrap();
    drop(journal);

    sim.reboot();
    let mut second = input(&sim, &mem);
    second.policy.cycles = NonZeroU32::new(2);
    let ended = run(second);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(ended.stop, Some(Stop::Cycles));
    let events = ended.journal.events();
    let restart = events.iter().find(|event| {
        matches!(&event.payload, Payload::CorePhase(phase)
            if phase.core == core(1)
                && phase.from == Some(CorePhase::AtLimit)
                && phase.to == CorePhase::Search)
    });
    let restart = restart.expect("core 1 restarts its search");
    assert_eq!(restart.cause, [reset]);
    assert!(restart.seq > reset);
    let last = phases_of(&ended.journal, core(1)).pop().unwrap();
    assert_eq!((last.1, last.2), (CorePhase::AtLimit, limit));
    let untouched = phases_of(&ended.journal, core(0));
    assert!(
        untouched
            .iter()
            .all(|(from, to, _)| !(*from == Some(CorePhase::AtLimit) && *to == CorePhase::Search))
    );
    let clean_cycles = events
        .iter()
        .filter(|event| {
            matches!(&event.payload, Payload::CheckingCycle(cycle)
                if cycle.event == CycleEvent::End && cycle.passed && cycle.full)
                && event.seq > reset
        })
        .count();
    assert!(
        clean_cycles > 0,
        "a clean full checking cycle ends after the reset"
    );
}

struct DurationTrials {
    inner: Box<dyn togi_machine::Trials>,
    ran: std::time::Duration,
    cancel: CancellationToken,
}

struct DurationRunning {
    inner: Box<dyn togi_machine::Running>,
    ran: std::time::Duration,
    cancel: CancellationToken,
}

impl togi_machine::Running for DurationRunning {
    fn started(&self) -> togi_machine::Started {
        self.inner.started()
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn togi_machine::Reporter,
    ) -> Result<togi_machine::TrialResult, togi_machine::Partial<togi_machine::TrialResult>> {
        let mut result = self.inner.wait(cancel, report)?;
        result.ran = self.ran;
        self.cancel.cancel();
        Ok(result)
    }

    fn stop(self: Box<Self>) -> Result<togi_machine::Stopped, togi_machine::Error> {
        self.inner.stop()
    }
}

impl togi_machine::Trials for DurationTrials {
    fn sweep(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<togi_machine::Swept, togi_machine::Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: togi_machine::TrialSpec,
        ready: togi_machine::Stopped,
    ) -> Result<Box<dyn togi_machine::Running>, togi_machine::Partial<Option<togi_machine::Stopped>>>
    {
        Ok(Box::new(DurationRunning {
            inner: self.inner.start(cancel, spec, ready)?,
            ran: self.ran,
            cancel: self.cancel.clone(),
        }))
    }

    fn ended(&mut self, id: &str) -> Result<(), togi_machine::Error> {
        self.inner.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<
        dyn Iterator<Item = Result<togi_machine::TrialConditions, togi_machine::SampleError>> + 'a,
    > {
        self.inner.samples(id)
    }
}

#[test_case::test_case(u64::from(u32::MAX) - 1, false; "below")]
#[test_case::test_case(u64::from(u32::MAX), false; "maximum")]
#[test_case::test_case(u64::from(u32::MAX) + 1, true; "overflow")]
fn trial_elapsed_seconds_are_checked(seconds: u64, refused: bool) {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    input.machine.trials = Box::new(DurationTrials {
        inner: input.machine.trials,
        ran: std::time::Duration::from_secs(seconds),
        cancel: input.cancel.clone(),
    });
    let ended = run(input);
    assert_eq!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, togi_session::SessionError::TrialDuration(_))),
        refused,
        "{:?}",
        ended.errors
    );
    let duration = ended
        .journal
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            Payload::TrialEnd(end) => Some(end.duration_s),
            _ => None,
        });
    assert_eq!(duration, u32::try_from(seconds).ok());
}

struct MonotonicClock {
    inner: Box<dyn togi_machine::Clock>,
    mono: std::time::Duration,
}

impl togi_machine::Clock for MonotonicClock {
    fn now(&self) -> jiff::Timestamp {
        self.inner.now()
    }

    fn monotonic(&self) -> std::time::Duration {
        self.mono
    }

    fn sleep(
        &mut self,
        cancel: &CancellationToken,
        duration: std::time::Duration,
    ) -> Result<(), togi_machine::Error> {
        self.inner.sleep(cancel, duration)
    }
}

#[test_case::test_case(-1, false; "below")]
#[test_case::test_case(0, false; "maximum")]
#[test_case::test_case(1, true; "overflow")]
fn trial_monotonic_nanoseconds_are_checked(delta: i8, refused: bool) {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    let max = std::time::Duration::from_nanos(u64::MAX);
    let nano = std::time::Duration::from_nanos(1);
    let mono = match delta {
        -1 => max - nano,
        0 => max,
        1 => max + nano,
        _ => unreachable!(),
    };
    input.machine.clock = Box::new(MonotonicClock {
        inner: input.machine.clock,
        mono,
    });
    input.machine.trials = Box::new(DurationTrials {
        inner: input.machine.trials,
        ran: std::time::Duration::from_secs(90),
        cancel: input.cancel.clone(),
    });
    let ended = run(input);
    assert_eq!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, togi_session::SessionError::Monotonic(_))),
        refused,
        "{:?}",
        ended.errors
    );
    let start = ended
        .journal
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            Payload::TrialStart(start) => start.window_start_ns,
            _ => None,
        });
    assert_eq!(start, u64::try_from(mono.as_nanos()).ok());
}

struct MonotonicKernel {
    inner: Box<dyn togi_machine::Kernel>,
    mono: std::time::Duration,
}

impl MonotonicKernel {
    fn mce(&self) -> togi_machine::Mce {
        togi_machine::Mce {
            cpu: togi_machine::CpuId::from(0),
            core: Some(core(0)),
            bank: 0,
            bank_type: togi_machine::BankType::Unknown,
            corrected: true,
            time: jiff::Timestamp::UNIX_EPOCH,
            monotonic: self.mono,
            lines: vec!["machine check".into()],
        }
    }
}

impl togi_machine::Kernel for MonotonicKernel {
    fn mces(
        &mut self,
        _: &togi_machine::BootId,
        _: std::time::Duration,
    ) -> Result<Vec<togi_machine::Mce>, togi_machine::Partial<Vec<togi_machine::Mce>>> {
        Ok(vec![self.mce()])
    }

    fn read_mces(
        &mut self,
        boot: &togi_machine::BootId,
        after: Option<&togi_machine::KernelCursor>,
    ) -> Result<togi_machine::KernelRead, togi_machine::Partial<Vec<togi_machine::Mce>>> {
        let mut read = self.inner.read_mces(boot, after)?;
        read.mces.push(self.mce());
        Ok(read)
    }

    fn reset_reason(
        &mut self,
        boot: &togi_machine::BootId,
    ) -> Result<togi_machine::ResetReason, togi_machine::Error> {
        self.inner.reset_reason(boot)
    }

    fn reset_reason_after(
        &mut self,
        boot: &togi_machine::BootId,
    ) -> Result<togi_machine::ResetReason, togi_machine::Error> {
        self.inner.reset_reason_after(boot)
    }

    fn saved_pstore(
        &mut self,
        boot: &togi_machine::BootId,
    ) -> Result<Option<togi_machine::PstoreRecord>, togi_machine::Error> {
        self.inner.saved_pstore(boot)
    }
}

#[test_case::test_case(-1, false; "below")]
#[test_case::test_case(0, false; "maximum")]
#[test_case::test_case(1, true; "overflow")]
fn kernel_monotonic_nanoseconds_are_checked(delta: i8, refused: bool) {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    let max = std::time::Duration::from_nanos(u64::MAX);
    let nano = std::time::Duration::from_nanos(1);
    let mono = match delta {
        -1 => max - nano,
        0 => max,
        1 => max + nano,
        _ => unreachable!(),
    };
    input.machine.kernel = Box::new(MonotonicKernel {
        inner: input.machine.kernel,
        mono,
    });
    input.machine.trials = Box::new(DurationTrials {
        inner: input.machine.trials,
        ran: std::time::Duration::from_secs(90),
        cancel: input.cancel.clone(),
    });
    let ended = run(input);
    assert_eq!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, togi_session::SessionError::Monotonic(_))),
        refused,
        "{:?}",
        ended.errors
    );
    let recorded = ended
        .journal
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            Payload::Mce(mce) => mce.monotonic_ns,
            _ => None,
        });
    assert_eq!(recorded, u64::try_from(mono.as_nanos()).ok());
}
