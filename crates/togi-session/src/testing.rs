//! A simulated machine, an in-memory journal and an appender over them, for the crate's tests.

use std::collections::BTreeMap;
use std::io;
use std::num::{NonZeroU16, NonZeroU32};

use jiff::tz::TimeZone;
use togi_config::Config;
use togi_defect::Entry;
use togi_journal::{
    Activity, Build, Event, MemFs, Payload, SCHEMA, Seq, SessionBaseline, SessionStart, StateDir,
    fs::Fault, fs::Op,
};
use togi_machine::{
    BootId, CancellationToken, Condition, CoreId, CoreSet, Machine, ObservedOffset, PerCore,
    Regime, Smu, SmuValidated, Stopped, TargetOffset, pick_workload,
};
use togi_machine::{Error, Partial, Running, Swept, TrialSpec, Trials};
use togi_render::Renderer;
use togi_sim::faults::{HookPhase, SmuOp, SmuOperation};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_tuner::{
    Action, ClassKey, Load, Profile, Requirement, RequirementKind, Secs, TrialContext, TrialPlan,
    Window, WorkloadChoice,
};
use togi_tuningboot::{Bootloader, Environment, SavedEntry};

use crate::appender::Appender;
use crate::boot::BootHandoff;
use crate::emergency::SweepCleared;
use crate::error::AppendFailure;
use crate::execute::TunerValues;
use crate::session::{ConfigSource, Control, RunInput, Session, StopPolicy};
use crate::write::{Hardware, ProfileRecord, ProfileRequest, apply_profile, checker};

pub(crate) struct Rig {
    pub sim: Simulator,
    pub mem: MemFs,
    pub appender: Appender,
    pub smu: Box<dyn Smu>,
    pub hardware: Hardware,
    pub stopped: Stopped,
    pub sweep: SweepCleared,
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
        fixes: 0,
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
        );
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
        let sweep = SweepCleared::of(&swept);
        let cores: CoreSet = topology.iter().map(|info| info.core).collect();
        let rig = Self {
            sim,
            mem,
            appender,
            smu,
            hardware: Hardware::new(cores),
            stopped: swept.stopped,
            sweep,
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
                &self.stopped,
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
        );
        Self {
            sim,
            mem: MemFs::new(),
            config: Config::default(),
            defects: &[],
        }
    }

    /// What a run on the simulator's current boot is given, with no bootloader, terminal or carry.
    pub(crate) fn input(&self) -> RunInput {
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
            policy: StopPolicy::default(),
            publication: crate::publication::PublicationPolicy::OnFinish,
            bootloader: None,
            prompt: None,
            defects: self.defects,
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

    pub(crate) fn open(&self) -> Session {
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

/// A bootloader environment that holds nothing.
#[derive(Default)]
pub(crate) struct MemBoot(BTreeMap<String, String>);

impl Environment for MemBoot {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        Ok(self.0.get(name).cloned())
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        for (name, value) in values {
            self.0.insert((*name).to_owned(), (*value).to_owned());
        }
        Ok(())
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        for name in names {
            self.0.remove(*name);
        }
        Ok(())
    }
}

impl Bootloader for MemBoot {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        let before = self.0.remove("saved_entry").unwrap_or_default();
        Ok(SavedEntry {
            before,
            after: String::new(),
        })
    }
}

/// Trials whose sweep fails with `error`; everything else is the simulator's.
pub(crate) struct SweepFails(pub Box<dyn Trials>, pub fn() -> Error);

impl Trials for SweepFails {
    fn sweep(&mut self, _cancel: &CancellationToken) -> Result<Swept, Error> {
        Err((self.1)())
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        self.0.start(cancel, spec, ready)
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        self.0.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = togi_machine::TrialConditions> + 'a> {
        self.0.samples(id)
    }
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
    let mut check = checker(session.appender.tuner());
    apply_profile(
        &mut session.appender,
        &mut *session.smu,
        session.stopped.as_ref().unwrap(),
        &mut session.hardware,
        ProfileRequest {
            target: &target,
            causes: &[],
            record: ProfileRecord::Applied(Condition::Together),
        },
        &mut check,
    )
    .unwrap();
}

pub(crate) fn swept(bench: &Bench) -> Session {
    let mut session = bench.open();
    assert_eq!(session.start().unwrap(), Control::Continue);
    assert_eq!(session.sweep().unwrap(), Control::Continue);
    session
}

/// A host whose SMU validation fails with `error`; everything else is the simulator's.
pub(crate) struct ValidationFails(pub Box<dyn togi_machine::Host>, pub fn() -> Error);

impl togi_machine::Host for ValidationFails {
    fn boot_id(&mut self) -> Result<BootId, Error> {
        self.0.boot_id()
    }

    fn topology(&mut self) -> Result<Vec<togi_machine::CoreInfo>, Error> {
        self.0.topology()
    }

    fn validate_smu(&mut self) -> Result<SmuValidated, Error> {
        Err((self.1)())
    }

    fn bios_context(&mut self) -> Result<togi_machine::BiosContext, Error> {
        self.0.bios_context()
    }

    fn ranking(
        &mut self,
    ) -> Result<Vec<togi_machine::CoreRank>, Partial<Vec<togi_machine::CoreRank>>> {
        self.0.ranking()
    }

    fn preflight(&mut self) -> Vec<togi_machine::Check> {
        self.0.preflight()
    }

    fn watchdog(&mut self) -> togi_machine::Check {
        self.0.watchdog()
    }
}

pub(crate) fn begun(bench: &Bench) -> Session {
    let mut session = swept(bench);
    assert_eq!(session.begin().unwrap(), Control::Continue);
    session
}

/// The search trial the tuner would ask for next on `core`, built the way the tuner builds it.
pub(crate) fn search_plan(session: &Session, core: CoreId, regime: Regime) -> TrialPlan {
    let topology = session.appender().tuner().topology().unwrap();
    let offset = session
        .appender()
        .journal()
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            Payload::CorePhase(phase) if phase.core == core => Some(phase.offset),
            _ => None,
        })
        .unwrap();
    let duration = Secs::new(NonZeroU32::new(session.config.search_trial().get()).unwrap());
    let rotation = session.appender().recovery().rotation(Some(core), regime);
    let workload = pick_workload(regime, usize::try_from(rotation).unwrap());
    TrialPlan {
        regime,
        workload: WorkloadChoice::Rotate,
        load: Load::Single(core),
        condition: Condition::Alone,
        activity: Activity::Search,
        profile: Profile::alone(topology, core, offset),
        duration,
        context: TrialContext::Search {
            core,
            regime,
            offset,
        },
        requirement: Requirement {
            kind: RequirementKind::Search,
            class: ClassKey::new(workload.id(), std::iter::once(core).collect(), duration),
            window: Window::Live { since: None },
            needed: NonZeroU16::MIN,
            step: None,
            part: None,
        },
        retry: false,
        cause: Vec::new(),
    }
}

pub(crate) fn tuned_values(session: &Session) -> Profile {
    Profile::zero(session.appender().tuner().topology().unwrap())
}

pub(crate) fn run_action(session: &mut Session, action: Action) -> crate::session::Outcome {
    let profile = tuned_values(session);
    session.execute(
        action,
        &TunerValues {
            profile: Some(&profile),
            profile_seq: None,
            clean_cycles: 0,
            first_result: false,
        },
    )
}

pub(crate) fn run_plan(session: &mut Session, plan: TrialPlan) -> crate::session::Outcome {
    run_action(session, Action::RunTrial(plan))
}

/// A host whose topology or BIOS context cannot be read; everything else is the simulator's.
pub(crate) struct ReadFails {
    pub inner: Box<dyn togi_machine::Host>,
    pub topology: Option<fn() -> Error>,
    pub context: Option<fn() -> Error>,
}

impl togi_machine::Host for ReadFails {
    fn boot_id(&mut self) -> Result<BootId, Error> {
        self.inner.boot_id()
    }

    fn topology(&mut self) -> Result<Vec<togi_machine::CoreInfo>, Error> {
        match self.topology {
            Some(error) => Err(error()),
            None => self.inner.topology(),
        }
    }

    fn validate_smu(&mut self) -> Result<SmuValidated, Error> {
        self.inner.validate_smu()
    }

    fn bios_context(&mut self) -> Result<togi_machine::BiosContext, Error> {
        match self.context {
            Some(error) => Err(error()),
            None => self.inner.bios_context(),
        }
    }

    fn ranking(
        &mut self,
    ) -> Result<Vec<togi_machine::CoreRank>, Partial<Vec<togi_machine::CoreRank>>> {
        self.inner.ranking()
    }

    fn preflight(&mut self) -> Vec<togi_machine::Check> {
        self.inner.preflight()
    }

    fn watchdog(&mut self) -> togi_machine::Check {
        self.inner.watchdog()
    }
}

/// A bootloader whose saved entry cannot be cleared.
#[derive(Default)]
pub(crate) struct ClearFails(pub MemBoot);

impl Environment for ClearFails {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        self.0.get(name)
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        self.0.set(values)
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        self.0.unset(names)
    }
}

impl Bootloader for ClearFails {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        Err(io::Error::other("GRUB environment unavailable"))
    }
}
