use std::num::NonZeroU32;
use std::time::Duration;

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::fs::FileKind;
use togi_journal::{Build, Event, EventKind, FixStamp, MemFs, SCHEMA, StateDir};
use togi_machine::{
    BankType, CancellationToken, ControllerMode, CoreId, ResetKind, TargetOffset, TrialSpec,
};
use togi_render::Renderer;
use togi_session::ConfigSource;
use togi_sim::{
    History, MachineSource, ScriptedMachine, ScriptedMce, ScriptedTopology, ScriptedVoltageSamples,
    Simulator, TrialOutcome,
};
use togi_simrun::{
    Finished, Observe, Observer, Persistence, Replay, SimInput, SimOutcome, events, simulate,
};

pub(crate) const STATE: &str = "/state";

/// How long the simulated operator lets a fixture run before ending its night.
pub(crate) const NIGHT: SignedDuration = SignedDuration::from_hours(24);

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

pub(crate) fn core(id: i64) -> CoreId {
    CoreId::new(id).unwrap()
}

pub(crate) fn voltage_samples(
    outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
) -> ScriptedMachine {
    static SAMPLES: ScriptedVoltageSamples = ScriptedVoltageSamples {
        requests_v: [
            1.07, 1.08, 1.09, 1.10, 1.11, 1.12, 1.13, 1.14, 1.15, 1.16, 1.17, 1.18, 1.19, 1.20,
            1.21, 1.22,
        ],
        ccd_mhz: [4_400, 4_300],
    };
    ScriptedMachine::sixteen_with_voltage_samples(TargetOffset::ZERO, outcome, &SAMPLES)
}

pub(crate) fn passing(topology: ScriptedTopology) -> ScriptedMachine {
    ScriptedMachine::new(topology, TargetOffset::ZERO, |_, _| TrialOutcome::Pass)
}

pub(crate) fn scripted_simulator(script: ScriptedMachine, seed: u64) -> Simulator {
    Simulator::start(MachineSource::Scripted(script), seed, History::default()).unwrap()
}

pub(crate) fn four_core_simulator(seed: u64) -> Simulator {
    scripted_simulator(passing(ScriptedTopology::FourCores), seed)
}

/// The four in-flight watchdog resets used by the boot cap and cross-process witnesses.
pub(crate) fn reset_schedule(spec: &TrialSpec) -> Option<(Duration, ScriptedMce)> {
    let ControllerMode::Managed { rotation, .. } = spec.controller;
    let (nanos, id, bank, bank_type) = match rotation {
        65 => (4_695_930, 0, 2, BankType::L2Cache),
        133 => (205_424, 2, 2, BankType::L2Cache),
        134 => (433_244_441, 0, 6, BankType::L3Cache),
        202 => (1_424_415_247, 2, 3, BankType::DecodeUnit),
        _ => return None,
    };
    Some((
        Duration::from_nanos(nanos),
        ScriptedMce {
            core: core(id),
            bank,
            bank_type,
        },
    ))
}

pub(crate) fn watchdog_resets() -> ScriptedMachine {
    ScriptedMachine::new(
        ScriptedTopology::FourCores,
        TargetOffset::ZERO,
        |spec, _| match reset_schedule(spec) {
            Some((at, _)) => TrialOutcome::Reset {
                stalled: None,
                kind: ResetKind::Watchdog,
                at,
            },
            None => TrialOutcome::Pass,
        },
    )
}

pub(crate) fn rebooting_simulator() -> Simulator {
    scripted_simulator(watchdog_resets(), 1)
}

pub(crate) fn nz(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

pub(crate) fn input(simulator: Simulator, persistence: Persistence) -> SimInput {
    let stop_at = Some(simulator.now() + NIGHT);
    SimInput {
        config: Config::default(),
        source: ConfigSource::default(),
        build: build(),
        simulator,
        persistence,
        replay: Replay::Warm,
        verify_every: Some(nz(1)),
        max_boots: nz(1000),
        stop_at,
        write_samples: false,
        defects: togi_defect::KNOWN_DEFECTS,
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: None,
        dump_to: None,
        adapt_trials: None,
    }
}

/// A simulated machine and the in-memory state directory its simulation keeps across runs.
pub(crate) struct Bench {
    pub sim: Simulator,
    pub fs: MemFs,
}

impl Bench {
    pub(crate) fn scripted(script: ScriptedMachine, seed: u64) -> Self {
        Self::with_simulator(scripted_simulator(script, seed))
    }

    pub(crate) fn four_cores(seed: u64) -> Self {
        Self::with_simulator(four_core_simulator(seed))
    }

    fn with_simulator(sim: Simulator) -> Self {
        Self {
            sim,
            fs: MemFs::new(),
        }
    }

    pub(crate) fn persistence(&self) -> Persistence {
        Persistence::Memory(self.fs.clone())
    }

    pub(crate) fn input(&self) -> SimInput {
        input(self.sim.clone(), self.persistence())
    }

    /// A fresh simulated process on the same machine, starting from the journal's recorded history.
    pub(crate) fn from_history(&self, seed: u64) -> Self {
        let history = togi_simrun::history_in(&self.dir()).unwrap();
        Self {
            sim: Simulator::start(self.sim.source(), seed, history).unwrap(),
            fs: self.fs.clone(),
        }
    }

    pub(crate) fn events(&self) -> Vec<Event> {
        events(&self.persistence()).unwrap()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "tests read an in-memory state directory"
    )]
    pub(crate) fn dir(&self) -> StateDir {
        StateDir::simulated(self.fs.clone()).unwrap()
    }

    pub(crate) fn recorded_config(&self, fresh: Config) -> Config {
        togi_simrun::recorded_config(&self.persistence(), fresh).unwrap()
    }

    /// A copy of the state directory with its own filesystem, for resuming the same history twice.
    pub(crate) fn fork(&self, seed: u64) -> Self {
        let copy = MemFs::new();
        for (path, kind) in self.fs.entries() {
            if kind == FileKind::File {
                copy.put(&path, self.fs.contents(&path).unwrap());
            }
        }
        Self {
            sim: self.sim.clone(),
            fs: copy,
        }
        .from_history(seed)
    }
}

/// Runs to the end of the simulation and returns how its last run ended; a capped simulation is a failure.
pub(crate) fn finish(input: SimInput) -> Finished {
    match simulate(input).unwrap() {
        SimOutcome::Stopped(finished) => *finished,
        SimOutcome::Capped { boots } => panic!("capped after {boots} boots"),
    }
}

/// Stops the run after the first event of `kind`.
pub(crate) fn until_kind(kind: EventKind) -> Observer {
    Box::new(move |event| {
        if event.kind() == kind {
            Observe::Stop
        } else {
            Observe::Continue
        }
    })
}

/// Crashes the machine once, at the `nth` (1 is the first) appended event of `kind`.
pub(crate) fn crash_at_nth(sim: &Simulator, kind: EventKind, nth: usize) -> Observer {
    let sim = sim.clone();
    let mut seen = 0;
    Box::new(move |event| {
        if event.kind() == kind {
            seen += 1;
            if seen == nth {
                sim.crash();
            }
        }
        Observe::Continue
    })
}

/// Crashes the machine at each of the first `count` appended events of `kind`, so each of the first boots crashes.
pub(crate) fn crash_first(sim: &Simulator, kind: EventKind, count: usize) -> Observer {
    let sim = sim.clone();
    let mut seen = 0;
    Box::new(move |event| {
        if event.kind() == kind && seen < count {
            seen += 1;
            sim.crash();
        }
        Observe::Continue
    })
}
