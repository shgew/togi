//! A simulated machine, an in-memory journal and an appender over them, for the crate's tests.

use std::io;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use togi_config::Config;
use togi_defect::Entry;
use togi_journal::{
    Build, Event, FixStamp, MemFs, Payload, SCHEMA, Seq, SessionBaseline, SessionStart, StateDir,
    fs::Fault, fs::Op,
};
use togi_machine::{
    BootId, CancellationToken, Condition, CoreId, CoreSet, Machine, ObservedOffset, PerCore, Smu,
    SmuValidated, TargetOffset,
};
use togi_render::Renderer;
use togi_sim::faults::{HookPhase, SmuOp, SmuOperation};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};

use crate::appender::Appender;
use crate::boot::BootHandoff;
use crate::emergency::SweepCleared;
use crate::error::AppendFailure;
use crate::session::{ConfigSource, Control, Lifecycle, RunInput, Session};
use crate::write::{Hardware, ProfileRecord, ProfileRequest, apply_profile};

pub(crate) struct Rig {
    pub sim: Simulator,
    pub mem: MemFs,
    pub appender: Appender<'static>,
    pub smu: Box<dyn Smu>,
    pub hardware: Hardware,
    /// Swept, with the proof that no workload runs.
    pub lifecycle: Lifecycle,
    pub validated: SmuValidated,
    pub boot: BootId,
}

#[expect(
    clippy::disallowed_methods,
    reason = "session tests keep the journal in memory"
)]
pub(crate) fn simulated(mem: &MemFs) -> StateDir {
    StateDir::simulated(mem.clone()).unwrap()
}

pub(crate) fn core(n: i64) -> CoreId {
    CoreId::new(n).unwrap()
}

pub(crate) fn target(n: i64) -> TargetOffset {
    TargetOffset::new(n).unwrap()
}

pub(crate) fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: 1,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: 1,
    }
}

impl Rig {
    pub(crate) fn new(cores: i64) -> Self {
        Self::defined(&MachineDefinition {
            cores,
            ..MachineDefinition::default()
        })
    }

    pub(crate) fn defined(definition: &MachineDefinition) -> Self {
        let (rig, first) = Self::build(definition, None, |_| {});
        first.unwrap();
        rig
    }

    /// A rig whose first append, `session.start`, is made after `setup` ran against the journal's filesystem and
    /// with `handoff` attached; the result of that append is returned beside the rig.
    pub(crate) fn custom(
        cores: i64,
        handoff: Option<BootHandoff>,
        setup: impl FnOnce(&MemFs),
    ) -> (Self, Result<Seq, AppendFailure>) {
        let definition = MachineDefinition {
            cores,
            ..MachineDefinition::default()
        };
        Self::build(&definition, handoff, setup)
    }

    fn build(
        definition: &MachineDefinition,
        handoff: Option<BootHandoff>,
        setup: impl FnOnce(&MemFs),
    ) -> (Self, Result<Seq, AppendFailure>) {
        let model = Model::new(definition, 1).unwrap();
        let sim = Simulator::new(
            model,
            RunSettings {
                seed: 1,
                ..RunSettings::default()
            },
        )
        .unwrap();
        let Machine {
            clock,
            smu,
            mut host,
            mut trials,
            kernel: _,
        } = sim.seams();
        let boot = host.boot_id().unwrap();
        let topology = host.topology().unwrap();
        let validated = host.validate_smu().unwrap();
        let mem = MemFs::new();
        let journal = simulated(&mem).open_at(boot.clone(), sim.now()).unwrap();
        let mut appender =
            Appender::resume(journal, clock, Renderer::default(), TimeZone::UTC).unwrap();
        if let Some(handoff) = handoff {
            appender = appender.with_handoff(handoff);
        }
        setup(&mem);
        let first = appender.append(
            Payload::SessionStart(SessionStart {
                build: build(),
                session: "20260101T000000Z".parse().unwrap(),
                cores: topology.clone(),
            }),
            &[],
        );
        let swept = trials.sweep(&CancellationToken::new()).unwrap();
        let cores: CoreSet = topology.iter().map(|info| info.core).collect();
        let rig = Self {
            sim,
            mem,
            appender,
            smu,
            hardware: Hardware::new(cores),
            lifecycle: Lifecycle::swept(SweepCleared::of(&swept), swept.stopped),
            validated,
            boot,
        };
        (rig, first)
    }

    pub(crate) fn events(&self) -> &[Event] {
        self.appender.journal().events()
    }

    pub(crate) fn kinds(&self) -> Vec<&'static str> {
        self.events()
            .iter()
            .map(|event| event.kind().as_str())
            .collect()
    }

    pub(crate) fn cleared(&self) -> SweepCleared {
        let Lifecycle::Swept(swept) = &self.lifecycle else {
            panic!("the rig is swept");
        };
        swept.cleared
    }

    pub(crate) fn offset(&mut self, n: i64) -> i64 {
        self.smu.offset(core(n)).unwrap().0
    }

    /// Zeroes and reads back every core, so every slot is known.
    pub(crate) fn start_at_zero(&mut self) {
        let checked = self.hardware.zero_all();
        drop(
            crate::write::apply(
                &mut self.appender,
                &mut *self.smu,
                self.lifecycle.stopped().unwrap(),
                checked,
                &[],
            )
            .unwrap(),
        );
    }

    /// Fails the `nth` write the journal makes from now on.
    pub(crate) fn fail_journal_write(&self, nth: u64) {
        self.mem.inject(Fault::before(Op::Write, nth));
    }

    /// Makes the first SMU access of `op` in `phase` fail as a machine crash.
    pub(crate) fn crash_at(&self, op: SmuOp, phase: HookPhase) {
        let mut pending = true;
        self.sim.interrupt_smu(move |operation: &SmuOperation| {
            if pending && operation.op == op && operation.phase == phase {
                pending = false;
                return Err(togi_machine::Error::Crashed);
            }
            Ok(())
        });
    }
}

/// The appender's incremental folds are what a fresh fold of the journal gives.
pub(crate) fn assert_fold_consistent(appender: &Appender) {
    appender
        .capsule()
        .unwrap()
        .verify(appender.journal().events())
        .unwrap();
}

/// A simulated machine and an in-memory journal directory, from which each run opens its own session.
pub(crate) struct Bench {
    pub sim: Simulator,
    pub mem: MemFs,
    pub config: Config,
    pub defects: &'static [Entry],
}

impl Bench {
    pub(crate) fn new(cores: i64) -> Self {
        Self::defined(&MachineDefinition {
            cores,
            ..MachineDefinition::default()
        })
    }

    pub(crate) fn defined(definition: &MachineDefinition) -> Self {
        let sim = Simulator::new(
            Model::new(definition, 1).unwrap(),
            RunSettings {
                seed: 1,
                ..RunSettings::default()
            },
        )
        .unwrap();
        Self {
            sim,
            mem: MemFs::new(),
            config: Config::default(),
            defects: togi_defect::KNOWN_DEFECTS,
        }
    }

    /// What a run on the simulator's current boot is given, with no bootloader, terminal or carry.
    pub(crate) fn input(&self) -> RunInput<'static> {
        let mut input = self.input_unattested();
        let firmware = input.machine.host.bios_context().unwrap();
        if input.journal.events().is_empty() {
            let session = input.journal.state_dir().session_id(self.sim.now()).unwrap();
            let cores = input.machine.host.topology().unwrap();
            input.journal.append(
                togi_journal::Stamp { time: self.sim.now(), mono: None },
                SessionStart { build: build(), session, cores }.into(),
                &[],
            ).unwrap();
        }
        if !input.journal.events().iter().any(|event| {
            matches!(&event.payload, Payload::StockAttested(value) if value.firmware == firmware)
        }) {
            input.journal.append(
                togi_journal::Stamp { time: self.sim.now(), mono: None },
                togi_journal::StockAttested { firmware }.into(),
                &[],
            ).unwrap();
        }
        input
    }

    /// A real unattended run cannot manufacture the operator's answer.
    pub(crate) fn input_unattested(&self) -> RunInput<'static> {
        let mut machine = self.sim.seams();
        let boot = machine.host.boot_id().unwrap();
        let journal = simulated(&self.mem)
            .open_at(boot.clone(), self.sim.now())
            .unwrap();
        RunInput {
            config: self.config.clone(),
            source: ConfigSource::default(),
            build: build(),
            boot,
            journal,
            machine,
            publication: crate::publication::PublicationPolicy::OnFinish,
            bootloader: None,
            defects: self.defects,
            resume: None,
            import: None,
            log: None,
            on_append: None,
            renderer: Renderer::default(),
            zone: TimeZone::UTC,
            cancel: CancellationToken::new(),
            report: Box::new(io::sink()),
        }
    }

    /// When the operator ends a short night: room for one mapping trial and not for a second.
    pub(crate) fn short_night(&self) -> Timestamp {
        self.sim.now() + SignedDuration::from_secs(150)
    }

    /// What a run is given whose operator ends it, as F10 or a stop signal would, at the first event appended at
    /// or after `at`.
    pub(crate) fn input_ending_at(&self, at: Timestamp) -> RunInput<'static> {
        let mut input = self.input();
        let cancel = input.cancel.clone();
        input.on_append = Some(Box::new(move |event| {
            if event.time >= at {
                cancel.cancel();
            }
        }));
        input
    }

    pub(crate) fn open(&self) -> Session<'static> {
        match Session::open(self.input()) {
            Ok(session) => session,
            Err(ended) => panic!("open: {:?}", ended.errors),
        }
    }
}

pub(crate) fn kinds_of(appender: &Appender) -> Vec<&'static str> {
    appender
        .journal()
        .events()
        .iter()
        .map(|event| event.kind().as_str())
        .collect()
}

#[test]
fn rig_starts_a_session() {
    let rig = Rig::new(4);
    assert_eq!(rig.kinds(), ["session.start"]);
    assert_fold_consistent(&rig.appender);
}

pub(crate) fn offsets(values: &[i64]) -> PerCore<TargetOffset> {
    (0..)
        .zip(values)
        .map(|(n, &value)| (core(n), target(value)))
        .collect()
}

pub(crate) fn observed(values: &[i64]) -> PerCore<ObservedOffset> {
    (0..)
        .zip(values)
        .map(|(n, &value)| (core(n), ObservedOffset(value)))
        .collect()
}

pub(crate) fn recorded_baseline(session: &mut Session, values: &[i64]) {
    session
        .appender
        .append(
            Payload::SessionBaseline(SessionBaseline {
                offsets: observed(values),
            }),
            &[],
        )
        .unwrap();
}

pub(crate) fn put(session: &mut Session, values: &[i64]) {
    let target = offsets(values);
    apply_profile(
        &mut session.appender,
        &mut *session.smu,
        session.lifecycle.stopped().unwrap(),
        &mut session.hardware,
        ProfileRequest {
            target: &target,
            causes: &[],
            record: ProfileRecord::Applied(Condition::Together),
        },
    )
    .unwrap();
}

pub(crate) fn swept(bench: &Bench) -> Session<'static> {
    let mut session = bench.open();
    assert_eq!(session.start().unwrap(), Control::Continue);
    assert_eq!(session.sweep().unwrap(), Control::Continue);
    session
}

pub(crate) fn begun(bench: &Bench) -> Session<'static> {
    let mut session = swept(bench);
    assert_eq!(session.begin().unwrap(), Control::Continue);
    session
}

/// A session on `bench` that ran one cycle and then had a day observer record a boot start, a checkpoint and a
/// clean stop, closed: the journal a fresh transition archives. Returns its id and the BIOS context it recorded.
pub(crate) fn finished_with_day_records(
    bench: &Bench,
) -> (togi_journal::SessionId, togi_machine::BiosContext) {
    let input = bench.input_ending_at(bench.short_night());
    let mut journal = crate::run(input).journal;
    let events = journal.events();
    let Payload::SessionStart(start) = &events[0].payload else {
        panic!("{:?}", events[0].payload);
    };
    let session = start.session;
    let context = events
        .iter()
        .find_map(|event| {
            if let Payload::SessionContext(context) = &event.payload {
                Some(context.bios_context())
            } else {
                None
            }
        })
        .unwrap();
    let stamp = togi_journal::Stamp {
        time: bench.sim.now(),
        mono: None,
    };
    let offsets: PerCore<ObservedOffset> = [(CoreId::new(0).unwrap(), ObservedOffset(0))]
        .into_iter()
        .chain([(CoreId::new(1).unwrap(), ObservedOffset(0))])
        .collect();
    for payload in [
        Payload::DayBootStart(togi_journal::DayBootStart {
            build: build(),
            bios: context.clone(),
            offsets,
            watchdog: None,
        }),
        Payload::DayCheckpoint(togi_journal::DayCheckpoint {
            applied_secs: 60,
            load_mix: togi_journal::LoadMix::default(),
            watchdog: None,
        }),
        Payload::DayCleanStop(togi_journal::DayCleanStop {}),
    ] {
        journal.append(stamp, payload, &[]).unwrap();
    }
    drop(journal.close().unwrap());
    (session, context)
}
