//! A simulated machine, an in-memory journal and an appender over them, for the crate's tests.

use jiff::tz::TimeZone;
use togi_journal::{
    Build, Event, MemFs, Payload, SCHEMA, Seq, SessionStart, StateDir, fs::Fault, fs::Op,
};
use togi_machine::{
    BootId, CancellationToken, CoreId, CoreSet, Machine, Smu, SmuValidated, Stopped, TargetOffset,
};
use togi_render::Renderer;
use togi_sim::faults::{HookPhase, SmuOp, SmuOperation};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};

use crate::appender::Appender;
use crate::boot::BootHandoff;
use crate::emergency::SweepCleared;
use crate::error::AppendFailure;
use crate::recovery::RecoveryState;
use crate::write::Hardware;

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
fn simulated(mem: &MemFs) -> StateDir {
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
        let (rig, first) = Self::custom(cores, None, |_| {});
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
        let model = Model::new(&definition, 1).unwrap();
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

/// The appender's incremental fold is what a fresh fold of the journal gives.
pub(crate) fn assert_fold_consistent(appender: &Appender) {
    let fresh = RecoveryState::replay(appender.journal().events()).unwrap();
    assert_eq!(&fresh, appender.recovery());
}

#[test]
fn rig_starts_a_session() {
    let rig = Rig::new(4);
    assert_eq!(rig.kinds(), ["session.start"]);
    assert_fold_consistent(&rig.appender);
}
