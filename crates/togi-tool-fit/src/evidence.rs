use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use togi_facts::{Decisive, TrialClass};
use togi_machine::{BiosContext, CoreId, Regime};
use togi_sim::definition::{BiosContextDef, CoreDef, JointDef, ModelDef};
use togi_sim::{Load, MachineDefinition};
use togi_tool_trialfacts::{Class, Profile, ProfileClassKey, TrialFact};

use crate::error::{EvidenceError, Problem};

pub(crate) const DEEPEST: i64 = -50;

/// The trial class of a decisive trial fact. [`decisive`] has refused every other kind.
pub(crate) fn trial_class(fact: &TrialFact) -> Option<TrialClass> {
    match fact.class {
        Class::Trial(class) => Some(class),
        Class::Idle { .. } => None,
    }
}

pub(crate) fn is_failure(fact: &TrialFact) -> bool {
    matches!(fact.outcome, Decisive::Failure { .. })
}

pub(crate) fn regime_of(fact: &TrialFact) -> Regime {
    fact.class.regime()
}

pub(crate) fn load_of(class: &TrialClass) -> Load<'_> {
    Load {
        regime: class.workload.regime(),
        workload: class.workload.as_str(),
        cores: class.loaded,
        duration: Duration::from_secs(u64::from(class.duration_s)),
    }
}

/// The decisive trials of an extract, checked to describe one machine: failures outside any trial have no
/// exposure to fit and are left out.
pub(crate) fn decisive(records: &[TrialFact]) -> Result<Vec<TrialFact>, EvidenceError> {
    let mut out = Vec::new();
    let mut shape: Option<(usize, Option<&BiosContext>)> = None;
    for fact in records {
        let Some(class) = trial_class(fact) else {
            continue;
        };
        let invalid = |problem| EvidenceError::Invalid {
            session: fact.session,
            seq: fact.seq,
            problem,
        };
        let found = fact.profile.as_slice().len();
        let (cores, context) = *shape.get_or_insert((found, fact.context.as_ref()));
        if cores < 2 || cores % 2 != 0 {
            return Err(invalid(Problem::CoreCount(cores)));
        }
        if found != cores {
            return Err(invalid(Problem::ProfileSize { found, cores }));
        }
        if class.loaded.is_empty() {
            return Err(invalid(Problem::NoLoadedCore));
        }
        if class.duration_s == 0 {
            return Err(invalid(Problem::ZeroDuration));
        }
        if context != fact.context.as_ref() {
            return Err(EvidenceError::MixedContexts);
        }
        if let Some(core) = class.loaded.iter().find(|core| core.index() >= cores) {
            return Err(invalid(Problem::LoadedCore {
                core: core.get(),
                cores,
            }));
        }
        out.push(fact.clone());
    }
    if out.is_empty() {
        return Err(EvidenceError::NoTrials);
    }
    Ok(out)
}

/// Equal profile and class observations with their counts, in order of first appearance.
#[derive(Clone, Debug)]
pub(crate) struct Observation {
    pub profile: Profile,
    pub class: TrialClass,
    pub n: u32,
    pub k: u32,
}

impl Observation {
    pub(crate) fn offset(&self, core: usize) -> i64 {
        self.profile
            .as_slice()
            .get(core)
            .map_or(0, |offset| offset.get())
    }

    pub(crate) fn loads(&self, core: CoreId) -> bool {
        self.class.loaded.contains(core)
    }

    pub(crate) fn regime(&self) -> Regime {
        self.class.workload.regime()
    }
}

pub(crate) fn aggregate(records: &[TrialFact]) -> Vec<Observation> {
    let mut indices: HashMap<ProfileClassKey, usize> = HashMap::new();
    let mut out: Vec<Observation> = Vec::new();
    for fact in records {
        let Some(class) = trial_class(fact) else {
            continue;
        };
        let at = *indices
            .entry(ProfileClassKey::from(fact))
            .or_insert_with(|| {
                out.push(Observation {
                    profile: fact.profile,
                    class,
                    n: 0,
                    k: 0,
                });
                out.len() - 1
            });
        out[at].n += 1;
        if is_failure(fact) {
            out[at].k += 1;
        }
    }
    out
}

/// `records.len()` trials drawn with replacement, from the stream `seed` names.
pub(crate) fn bootstrap(records: &[TrialFact], seed: u64) -> Vec<TrialFact> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..records.len())
        .map(|_| records[rng.random_range(0..records.len())].clone())
        .collect()
}

pub(crate) fn bios_context_def(context: &BiosContext) -> BiosContextDef {
    BiosContextDef {
        bios_version: Some(context.bios_version.clone()),
        board: Some(context.board.clone()),
        cpu_model: Some(context.cpu_model.clone()),
        microcode: Some(context.microcode.clone()),
        boost_limit_mhz: Some(context.boost_limit_mhz),
    }
}

pub(crate) fn core_def(id: usize) -> CoreDef {
    CoreDef {
        id: i64::try_from(id).unwrap_or(i64::MAX),
        alone: [DEEPEST; 5],
        together: [DEEPEST; 7],
        flat: 0.0,
        idle: None,
        workload: BTreeMap::new(),
    }
}

/// The machine a fit starts from: every limit at the deepest offset, the hazard shape's starting values and
/// one all-member R7 joint per CCD that has a failure with every one of its cores below 0.
pub(crate) fn initial_config(records: &[TrialFact]) -> MachineDefinition {
    let cores = records
        .first()
        .map_or(0, |fact| fact.profile.as_slice().len());
    let mut cfg = MachineDefinition {
        cores: i64::try_from(cores).unwrap_or(i64::MAX),
        model: ModelDef {
            past_limit_rate: 0.015,
            growth: 2.0,
            near_limit_rate: 1e-7,
            ..ModelDef::default()
        },
        core: (0..cores).map(core_def).collect(),
        bios_context: records
            .first()
            .and_then(|fact| fact.context.as_ref())
            .map(bios_context_def),
        ..MachineDefinition::default()
    };
    for ccd in 0..2 {
        let half = cores / 2;
        let range = ccd * half..(ccd + 1) * half;
        let mut members: BTreeMap<i64, i64> = range
            .clone()
            .map(|core| (i64::try_from(core).unwrap_or(i64::MAX), DEEPEST))
            .collect();
        let mut found = false;
        for fact in records {
            if regime_of(fact) != Regime::R7 || !is_failure(fact) {
                continue;
            }
            let profile = fact.profile.as_slice();
            if !range.clone().all(|core| profile[core].get() < 0) {
                continue;
            }
            found = true;
            for core in range.clone() {
                if let Some(limit) = members.get_mut(&i64::try_from(core).unwrap_or(i64::MAX)) {
                    *limit = (*limit).max(profile[core].get());
                }
            }
        }
        if found {
            cfg.joint.push(JointDef {
                members,
                regimes: vec![Regime::R7],
                rate: Some(0.005),
                ..JointDef::default()
            });
        }
    }
    cfg
}
