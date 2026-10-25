//! The validated, resolved machine: defaults applied, limits seeded, every field checked.

use std::collections::BTreeMap;
use std::sync::Arc;
use togi_machine::{BiosContext, CoreId, Regime, ResetKind, Signal, TargetOffset, WorkloadId};

use crate::definition::{
    BiosContextDef, CcdDef, CoreDef, DEFAULT_BIOS_VERSION, DEFAULT_BOARD, DEFAULT_BOOST_LIMIT_MHZ,
    DEFAULT_CPU_MODEL, DEFAULT_MICROCODE, JointDef, MachineDefinition, ModelDef, ScriptDef,
    Weights,
};
use crate::error::{InvalidMachine, Path, Problem};
use crate::hazard::largest_limit_rate;
use crate::random::draw_limits;
use crate::vocab::{
    MODELED_REGIMES, ResetMix, SignalMix, modeled_as, regime_index, reset_slot, signal_slot,
};
use crate::voltage::Voltage;

const MAX_CORES: usize = 16;

/// A core's hidden limits: the deepest offset it passes, so a limit of -30 fails at -31.
/// A limit of 1 means offset 0 already fails.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// R1 to R5 with every other core at 0.
    pub alone: [i64; 5],
    /// R1 to R7 with other cores set; R8 reads R2's slot and R9 R7's ([`limit_index`](crate::limit_index)).
    pub together: [i64; 7],
    /// The limit of the core while unloaded.
    pub idle: Option<i64>,
    pub flat: f64,
    /// Limit per workload ID, sorted in catalog order.
    pub workload: Vec<(WorkloadId, i64)>,
}

/// The initial window in which every failure rate is multiplied by `1 + boost`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Onset {
    pub window_s: f64,
    pub boost: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub past_limit_rate: f64,
    pub growth: f64,
    pub near_limit_rate: f64,
    pub crash_mce: f64,
    pub core_local_bank: f64,
    pub onset: Onset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JointMember {
    pub core: CoreId,
    pub offset: TargetOffset,
}

/// A set of regimes; the empty set means every regime. A regime is also in the set when the regime it is
/// [modeled as](crate::limit_index) is: a joint that names R7 covers R9, and one that names R2 covers R8.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegimeSet(u16);

impl RegimeSet {
    fn from_regimes(regimes: &[Regime]) -> Self {
        Self(
            regimes
                .iter()
                .fold(0, |mask, &regime| mask | 1 << regime_index(regime)),
        )
    }

    pub const fn is_all(self) -> bool {
        self.0 == 0
    }

    pub fn covers(self, regime: Regime) -> bool {
        let has = |regime| self.0 & (1 << regime_index(regime)) != 0;
        self.is_all() || has(regime) || has(modeled_as(regime))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Joint {
    /// Members by ascending core. Never empty.
    pub members: Vec<JointMember>,
    pub regimes: RegimeSet,
    /// Failures per second while active, resolved against the model's `past_limit_rate`.
    pub rate: f64,
    pub after_s: f64,
    pub signal: Option<Signal>,
    pub crash_mce_core: Option<CoreId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Script {
    pub signal: Option<Signal>,
    pub at_s: f64,
    pub core: CoreId,
    pub reset: Option<ResetKind>,
    pub then_crash: bool,
}

#[derive(Debug)]
struct Inner {
    seed: u64,
    cores: usize,
    facts: Option<String>,
    bios: Vec<TargetOffset>,
    context: BiosContext,
    context_declared: bool,
    ranking: Option<Vec<i64>>,
    old_kernel: bool,
    params: Params,
    signals: SignalMix,
    regime_signals: [Option<SignalMix>; MODELED_REGIMES],
    reset: ResetMix,
    limits: Vec<Limits>,
    ccd: Option<CcdDef>,
    joints: Vec<Joint>,
    scripts: BTreeMap<String, Script>,
    voltage: Option<Voltage>,
}

/// An immutable simulated machine: what it is, with no host state. It answers every question about
/// failure rates ([`hazard`](Self::hazard), [`failure_probability`](Self::failure_probability),
/// [`sources`](Self::sources)), so it is also the predictor tools evaluate. Cloning shares the data.
#[derive(Clone, Debug)]
pub struct Model(Arc<Inner>);

impl Model {
    /// Validates every field of `def`, applies defaults and draws the hidden limits from `seed` when
    /// the definition has none. The first problem in document order is reported with its field path.
    pub fn new(def: &MachineDefinition, seed: u64) -> Result<Self, InvalidMachine> {
        let cores = resolve_cores(def.cores)?;
        let bios = resolve_bios(def.bios.as_deref(), cores)?;
        let ranking = match &def.ranking {
            Some(ranking) => {
                Path::root("ranking").length(cores, ranking.len())?;
                Some(ranking.clone())
            }
            None => None,
        };
        let model = Path::root("model");
        let (params, signals, regime_signals, reset) = resolve_model(&def.model, &model)?;
        let ccd = def.ccd.as_ref().map(resolve_ccd).transpose()?;
        let limits = if def.core.is_empty() {
            draw_limits(seed, cores)
        } else {
            resolve_limits(
                &def.core,
                cores,
                largest_limit_rate(
                    params.past_limit_rate,
                    params.growth,
                    params.near_limit_rate,
                ),
            )?
        };
        let joints = def
            .joint
            .iter()
            .enumerate()
            .map(|(i, joint)| resolve_joint(joint, i, cores, params.past_limit_rate))
            .collect::<Result<_, _>>()?;
        let scripts = resolve_scripts(&def.script, cores)?;
        let voltage = match &def.shared_voltage {
            Some(voltage) => {
                let path = Path::root("shared_voltage");
                if cores != MAX_CORES {
                    return Err(path.error(Problem::NeedsSixteenCores));
                }
                Some(Voltage::resolve(voltage, &path)?)
            }
            None => None,
        };
        Ok(Self(Arc::new(Inner {
            seed,
            cores,
            facts: def.facts.clone(),
            bios,
            context: resolve_context(def.bios_context.as_ref()),
            context_declared: def.bios_context.is_some(),
            ranking,
            old_kernel: def.old_kernel,
            params,
            signals,
            regime_signals,
            reset,
            limits,
            ccd,
            joints,
            scripts,
            voltage,
        })))
    }

    pub fn seed(&self) -> u64 {
        self.0.seed
    }

    pub fn cores(&self) -> usize {
        self.0.cores
    }

    /// The facts extract path as written in the machine file, relative to it.
    pub fn facts(&self) -> Option<&str> {
        self.0.facts.as_deref()
    }

    /// The offsets restored at every boot.
    pub fn bios(&self) -> &[TargetOffset] {
        &self.0.bios
    }

    pub fn bios_context(&self) -> &BiosContext {
        &self.0.context
    }

    /// Whether the machine file declares a BIOS context, rather than leaving the simulator's default.
    pub fn bios_context_declared(&self) -> bool {
        self.0.context_declared
    }

    /// The shallowest of a core's alone limits: the deepest offset it passes in every R1 to R5.
    pub fn alone_limit(&self, core: CoreId) -> i64 {
        self.0.limits[core.index()]
            .alone
            .iter()
            .copied()
            .max()
            .unwrap_or(-50)
    }

    pub fn ranking(&self) -> Option<&[i64]> {
        self.0.ranking.as_deref()
    }

    pub fn old_kernel(&self) -> bool {
        self.0.old_kernel
    }

    pub fn params(&self) -> &Params {
        &self.0.params
    }

    /// The signal weights of failures in `regime`: its own when the model lists one, else those of the regime it
    /// is modeled as (R8 as R2, R9 as R7) when listed, else the pooled mix.
    pub fn signal_mix(&self, regime: Regime) -> &SignalMix {
        let listed = |regime| self.0.regime_signals[regime_index(regime)].as_ref();
        listed(regime)
            .or_else(|| listed(modeled_as(regime)))
            .unwrap_or(&self.0.signals)
    }

    /// Whether any regime has its own signal mix. When one does, a loaded joint without a signal and a
    /// CCD failure draw their signal from the mix; otherwise they crash.
    pub fn has_regime_signals(&self) -> bool {
        self.0.regime_signals.iter().any(Option::is_some)
    }

    pub fn reset_mix(&self) -> &ResetMix {
        &self.0.reset
    }

    pub fn limits(&self) -> &[Limits] {
        &self.0.limits
    }

    pub fn joints(&self) -> &[Joint] {
        &self.0.joints
    }

    pub fn ccd(&self) -> Option<&CcdDef> {
        self.0.ccd.as_ref()
    }

    pub fn voltage(&self) -> Option<&Voltage> {
        self.0.voltage.as_ref()
    }

    pub fn scripts(&self) -> &BTreeMap<String, Script> {
        &self.0.scripts
    }

    pub fn script(&self, trial: &str) -> Option<&Script> {
        self.0.scripts.get(trial)
    }

    /// The CCD of `core`: cores are split in halves.
    pub fn ccd_of(&self, core: CoreId) -> usize {
        core.index() / (self.0.cores / 2)
    }
}

fn resolve_cores(cores: i64) -> Result<usize, InvalidMachine> {
    match usize::try_from(cores) {
        Ok(count) if (2..=MAX_CORES).contains(&count) && count % 2 == 0 => Ok(count),
        _ => Err(Path::root("cores").error(Problem::Topology(cores))),
    }
}

fn resolve_bios(bios: Option<&[i64]>, cores: usize) -> Result<Vec<TargetOffset>, InvalidMachine> {
    let Some(bios) = bios else {
        return Ok(vec![TargetOffset::ZERO; cores]);
    };
    let root = Path::root("bios");
    root.length(cores, bios.len())?;
    bios.iter()
        .enumerate()
        .map(|(core, &offset)| {
            root.index(core)
                .within(offset, -50, 0)
                .map(TargetOffset::clamped)
        })
        .collect()
}

fn resolve_context(def: Option<&BiosContextDef>) -> BiosContext {
    let default = BiosContextDef::default();
    let def = def.unwrap_or(&default);
    let text =
        |value: &Option<String>, default: &str| value.as_deref().unwrap_or(default).to_owned();
    BiosContext {
        bios_version: text(&def.bios_version, DEFAULT_BIOS_VERSION),
        board: text(&def.board, DEFAULT_BOARD),
        cpu_model: text(&def.cpu_model, DEFAULT_CPU_MODEL),
        microcode: text(&def.microcode, DEFAULT_MICROCODE),
        boost_limit_mhz: def.boost_limit_mhz.unwrap_or(DEFAULT_BOOST_LIMIT_MHZ),
    }
}

fn mix_weights<K: Copy + Ord, const N: usize>(
    path: &Path<'_>,
    weights: &Weights<K>,
    index: impl Fn(K) -> Option<usize>,
    name: impl Fn(K) -> &'static str,
) -> Result<[f64; N], InvalidMachine> {
    let mut out = [0.0; N];
    for (&kind, &weight) in &weights.0 {
        let at = path.key(name(kind));
        let slot = index(kind).ok_or_else(|| at.error(Problem::Unsupported))?;
        out[slot] = at.nonnegative(weight)?;
    }
    let total: f64 = out.iter().sum();
    if !total.is_finite() {
        return Err(path.error(Problem::NotFinite));
    }
    if total <= 0.0 {
        return Err(path.error(Problem::ZeroTotal));
    }
    Ok(out)
}

pub(crate) fn signal_mix(
    path: &Path<'_>,
    signals: &Weights<Signal>,
) -> Result<SignalMix, InvalidMachine> {
    mix_weights(path, signals, signal_slot, Signal::as_str).map(SignalMix::new)
}

type ModelParts = (
    Params,
    SignalMix,
    [Option<SignalMix>; MODELED_REGIMES],
    ResetMix,
);

fn resolve_model(model: &ModelDef, path: &Path<'_>) -> Result<ModelParts, InvalidMachine> {
    let growth_path = path.key("growth");
    let past_limit_rate = path
        .key("past_limit_rate")
        .nonnegative(model.past_limit_rate)?;
    let growth = growth_path.positive(model.growth)?;
    let near_limit_rate = path
        .key("near_limit_rate")
        .nonnegative(model.near_limit_rate)?;
    if !largest_limit_rate(past_limit_rate, growth, near_limit_rate).is_finite() {
        return Err(growth_path.error(Problem::Overflow));
    }
    let params = Params {
        past_limit_rate,
        growth,
        near_limit_rate,
        crash_mce: path.key("crash_mce").probability(model.crash_mce)?,
        core_local_bank: path
            .key("core_local_bank")
            .probability(model.core_local_bank)?,
        onset: Onset {
            window_s: path.key("onset_s").nonnegative(model.onset_s)?,
            boost: path.key("onset_boost").nonnegative(model.onset_boost)?,
        },
    };
    let signals = signal_mix(&path.key("signals"), &model.signals)?;
    let mut regime_signals: [Option<SignalMix>; MODELED_REGIMES] = Default::default();
    let regimes = path.key("regime_signals");
    for (regime, mix) in &model.regime_signals {
        let mix_path = regimes.key(regime.as_str());
        regime_signals[regime_index(*regime)] = Some(signal_mix(&mix_path, mix)?);
    }
    let reset = mix_weights(
        &path.key("reset"),
        &model.reset,
        |reset| Some(reset_slot(reset)),
        ResetKind::as_str,
    )
    .map(ResetMix::new)?;
    Ok((params, signals, regime_signals, reset))
}

fn resolve_ccd(ccd: &CcdDef) -> Result<CcdDef, InvalidMachine> {
    let path = Path::root("ccd");
    path.key("log_rate").finite(ccd.log_rate)?;
    path.key("slope").nonnegative(ccd.slope)?;
    for (i, &effect) in ccd.effect.iter().enumerate() {
        path.key("effect").index(i).finite(effect)?;
    }
    // The deepest mean applied depth is 25 counts past the exponent's reference, and the slope is not
    // negative. Finite parts can still sum to infinity or cancel to NaN, which no comparison catches.
    for effect in ccd.effect {
        let exponent = ccd.log_rate + effect + ccd.slope * 25.0;
        if !exponent.is_finite() || exponent > 700.0 {
            return Err(path.error(Problem::Overflow));
        }
    }
    Ok(ccd.clone())
}

fn core_id(path: &Path<'_>, value: i64, cores: usize) -> Result<CoreId, InvalidMachine> {
    let last = i64::try_from(cores).map_err(|_| path.error(Problem::Overflow))? - 1;
    path.within(value, 0, last)?;
    CoreId::new(value).map_err(|_| {
        path.error(Problem::OutOfRange {
            value,
            min: 0,
            max: last,
        })
    })
}

fn resolve_limits(
    defs: &[CoreDef],
    cores: usize,
    limit_rate: f64,
) -> Result<Vec<Limits>, InvalidMachine> {
    let mut slots: Vec<Option<Limits>> = vec![None; cores];
    let table = Path::root("core");
    for (i, def) in defs.iter().enumerate() {
        let path = table.index(i);
        let id = core_id(&path.key("id"), def.id, cores)?.index();
        if slots[id].is_some() {
            return Err(path.key("id").error(Problem::Duplicate));
        }
        let alone = path.key("alone");
        for (j, &limit) in def.alone.iter().enumerate() {
            alone.index(j).within(limit, -50, 1)?;
        }
        let together = path.key("together");
        for (j, &limit) in def.together.iter().enumerate() {
            together.index(j).within(limit, -50, 1)?;
        }
        let flat_path = path.key("flat");
        let flat = flat_path.nonnegative(def.flat)?;
        if !(flat + limit_rate).is_finite() {
            return Err(flat_path.error(Problem::Overflow));
        }
        if let Some(idle) = def.idle {
            path.key("idle").within(idle, -50, 1)?;
        }
        let workload = path.key("workload");
        for (id, &limit) in &def.workload {
            workload.key(id.as_str()).within(limit, -50, 1)?;
        }
        slots[id] = Some(Limits {
            alone: def.alone,
            together: def.together,
            idle: def.idle,
            flat,
            workload: def.workload.iter().map(|(&k, &v)| (k, v)).collect(),
        });
    }
    slots
        .into_iter()
        .enumerate()
        .map(|(core, slot)| slot.ok_or_else(|| table.error(Problem::MissingCore(core))))
        .collect()
}

fn resolve_joint(
    joint: &JointDef,
    index: usize,
    cores: usize,
    past_limit_rate: f64,
) -> Result<Joint, InvalidMachine> {
    let table = Path::root("joint");
    let path = table.index(index);
    let members_path = path.key("members");
    if joint.members.is_empty() {
        return Err(members_path.error(Problem::Empty));
    }
    let members = joint
        .members
        .iter()
        .map(|(&core, &offset)| {
            let member = members_path.number(core);
            Ok(JointMember {
                core: core_id(&member, core, cores)?,
                offset: TargetOffset::clamped(member.within(offset, -50, 0)?),
            })
        })
        .collect::<Result<_, InvalidMachine>>()?;
    let rate = match joint.rate {
        Some(rate) => path.key("rate").nonnegative(rate)?,
        None => past_limit_rate,
    };
    let after_s = path.key("after_s").nonnegative(joint.after_s)?;
    let signal =
        failure_signal(joint.signal).map_err(|problem| path.key("signal").error(problem))?;
    let crash_mce_core = joint
        .crash_mce_core
        .map(|core| core_id(&path.key("crash_mce_core"), core, cores))
        .transpose()?;
    Ok(Joint {
        members,
        regimes: RegimeSet::from_regimes(&joint.regimes),
        rate,
        after_s,
        signal,
        crash_mce_core,
    })
}

pub(crate) fn failure_signal(signal: Option<Signal>) -> Result<Option<Signal>, Problem> {
    match signal {
        Some(signal) if signal_slot(signal).is_none() => Err(Problem::Unsupported),
        other => Ok(other),
    }
}

fn resolve_scripts(
    defs: &[ScriptDef],
    cores: usize,
) -> Result<BTreeMap<String, Script>, InvalidMachine> {
    let table = Path::root("script");
    let mut scripts = BTreeMap::new();
    for (i, def) in defs.iter().enumerate() {
        let path = table.index(i);
        if def.trial.is_empty() {
            return Err(path.key("trial").error(Problem::Empty));
        }
        if scripts.contains_key(&def.trial) {
            return Err(path.key("trial").error(Problem::Duplicate));
        }
        let script = Script {
            signal: failure_signal(def.signal)
                .map_err(|problem| path.key("signal").error(problem))?,
            at_s: path.key("at_s").nonnegative(def.at_s)?,
            core: core_id(&path.key("core"), def.core, cores)?,
            reset: def.reset,
            then_crash: def.then_crash,
        };
        scripts.insert(def.trial.clone(), script);
    }
    Ok(scripts)
}
