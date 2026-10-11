use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use togi_machine::{
    BootId, CancellationToken, Condition, CoreId, CoreSet, CpuId, Error, Machine, Mce,
    ObservedOffset, Partial, Regime, Reporter, Running, Sample, Signal, Stopped, TargetOffset,
    TrialResult, TrialSpec, Workload, pick_workload,
};
use togi_sim::definition::{CoreDef, ModelDef, Weights};
use togi_sim::{InvalidMachine, Load, MachineDefinition, Model, RunSettings, Simulator, file};

pub(crate) fn machines_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench/machines")
}

pub(crate) fn committed() -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(machines_dir())
        .expect("machines directory")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    paths
}

pub(crate) fn core_def(id: i64, alone: i64, together: i64) -> CoreDef {
    CoreDef {
        id,
        alone: [alone; 5],
        together: [together; 7],
        flat: 0.0,
        idle: None,
        workload: BTreeMap::new(),
    }
}

pub(crate) fn flat_cores(cores: i64, alone: i64, together: i64) -> Vec<CoreDef> {
    (0..cores).map(|id| core_def(id, alone, together)).collect()
}

pub(crate) fn machine(cores: i64, core: Vec<CoreDef>) -> MachineDefinition {
    MachineDefinition {
        cores,
        core,
        ..MachineDefinition::default()
    }
}

pub(crate) fn model(definition: &MachineDefinition) -> Model {
    Model::new(definition, 1).expect("a valid machine")
}

pub(crate) fn invalid(definition: &MachineDefinition) -> InvalidMachine {
    Model::new(definition, 1).expect_err("an invalid machine")
}

pub(crate) fn offsets(values: &[i64]) -> Vec<TargetOffset> {
    values
        .iter()
        .map(|&value| TargetOffset::new(value).expect("an offset in range"))
        .collect()
}

pub(crate) fn cores(ids: &[i64]) -> CoreSet {
    ids.iter()
        .map(|&id| togi_machine::CoreId::new(id).expect("a core ID"))
        .collect()
}

pub(crate) fn load<'a>(regime: Regime, workload: &'a str, ids: &[i64], seconds: f64) -> Load<'a> {
    Load {
        regime,
        workload,
        cores: cores(ids),
        duration: Duration::from_secs_f64(seconds),
    }
}

pub(crate) fn committed_machine(name: &str) -> MachineDefinition {
    file::load(machines_dir().join(name)).expect("a committed machine")
}

pub(crate) fn r7_workload(index: usize) -> &'static str {
    pick_workload(Regime::R7, index).id
}

pub(crate) fn machine_dir_text(name: &str) -> String {
    std::fs::read_to_string(machines_dir().join(name)).expect("a committed machine")
}

pub(crate) fn core(id: i64) -> CoreId {
    CoreId::new(id).expect("a core")
}

pub(crate) fn offset(value: i64) -> TargetOffset {
    TargetOffset::new(value).expect("an offset in range")
}

/// A model whose one failure signal fires at once, past any limit.
pub(crate) fn sharp(signal: Signal) -> ModelDef {
    ModelDef {
        past_limit_rate: 1e6,
        signals: Weights::from([(signal, 1.0)]),
        ..ModelDef::default()
    }
}

#[derive(Default)]
pub(crate) struct Recorder {
    pub progress: Vec<String>,
    pub signals: Vec<(CoreId, Signal)>,
}

impl Reporter for Recorder {
    fn progress(&mut self, detail: &str) {
        self.progress.push(detail.to_owned());
    }

    fn sample(&mut self, _sample: Sample) {}

    fn signal(&mut self, core: CoreId, signal: Signal, _detail: &str) {
        self.signals.push((core, signal));
    }
}

pub(crate) fn spec(
    id: &str,
    regime: Regime,
    workload: &Workload,
    cores: &[i64],
    duration: Duration,
) -> TrialSpec {
    TrialSpec {
        id: id.to_owned(),
        regime,
        workload: workload.id(),
        condition: Condition::Together,
        cores: cores.iter().map(|&id| core(id)).collect(),
        cpus: cores
            .iter()
            .map(|&id| CpuId(u16::try_from(id).expect("a CPU")))
            .collect(),
        duration,
        index: 0,
        seed: 1,
    }
}

/// A trial of the first workload of `regime`.
pub(crate) fn basic(regime: Regime, cores: &[i64], duration: Duration) -> TrialSpec {
    spec("0001", regime, pick_workload(regime, 0), cores, duration)
}

pub(crate) fn load_of(spec: &TrialSpec) -> togi_sim::Load<'_> {
    togi_sim::Load {
        regime: spec.regime,
        workload: spec.workload.as_str(),
        cores: spec.cores.iter().copied().collect(),
        duration: spec.duration,
    }
}

pub(crate) type Waited = Result<TrialResult, Partial<TrialResult>>;

pub(crate) fn crashed(waited: &Waited) -> bool {
    matches!(
        waited,
        Err(Partial {
            error: Error::Crashed,
            ..
        })
    )
}

/// A simulator with its seams, and the plumbing to run trials through them.
pub(crate) struct Rig {
    pub sim: Simulator,
    pub machine: Machine,
}

impl Rig {
    pub(crate) fn new(definition: &MachineDefinition, seed: u64) -> Self {
        Self::with(
            model(definition),
            RunSettings {
                seed,
                ..RunSettings::default()
            },
        )
    }

    pub(crate) fn with(model: Model, settings: RunSettings) -> Self {
        let sim = Simulator::new(model, settings);
        let machine = sim.seams();
        Self { sim, machine }
    }

    pub(crate) fn proof(&mut self) -> Stopped {
        self.machine
            .trials
            .sweep(&CancellationToken::new())
            .expect("a sweep")
            .stopped
    }

    pub(crate) fn start(
        &mut self,
        spec: TrialSpec,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let ready = self.proof();
        self.machine
            .trials
            .start(&CancellationToken::new(), spec, ready)
    }

    pub(crate) fn run(&mut self, spec: TrialSpec) -> (Waited, Recorder) {
        let mut running = self
            .start(spec)
            .unwrap_or_else(|error| panic!("start: {error}"));
        let mut recorder = Recorder::default();
        let waited = running.wait(&CancellationToken::new(), &mut recorder);
        (waited, recorder)
    }

    /// Runs a trial that must come back with a result, not a crash.
    pub(crate) fn pass(&mut self, spec: TrialSpec) -> TrialResult {
        self.run(spec).0.unwrap_or_else(|error| panic!("{error}"))
    }

    pub(crate) fn set(&mut self, core: i64, value: i64) -> Result<(), Error> {
        self.machine.smu.set_offset(self::core(core), offset(value))
    }

    pub(crate) fn set_all(&mut self, value: i64) -> Result<(), Error> {
        self.machine.smu.set_all_offsets(offset(value))
    }

    pub(crate) fn read(&mut self, core: i64) -> Result<ObservedOffset, Error> {
        self.machine.smu.offset(self::core(core))
    }

    pub(crate) fn boot(&mut self) -> BootId {
        self.machine.host.boot_id().expect("a running machine")
    }

    pub(crate) fn mces(&mut self, boot: &BootId) -> Vec<Mce> {
        self.machine
            .kernel
            .mces(boot, Duration::ZERO)
            .expect("a known boot")
    }

    /// Zeroes every register, then sets one, and runs the first workload of `regime` on that core alone.
    pub(crate) fn alone(&mut self, core: i64, value: i64, regime: Regime, index: usize) -> Waited {
        self.set_all(0).expect("zero");
        self.set(core, value).expect("set");
        let mut spec = basic(regime, &[core], Duration::from_secs(90));
        spec.workload = pick_workload(regime, index).id();
        spec.condition = Condition::Alone;
        spec.index = index;
        self.run(spec).0
    }
}
