use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use togi_facts::Decisive;
use togi_sim::MachineDefinition;
use togi_sim::replay::{InvalidFact, Replay, ReplayFact, ReplayOutcome};

use crate::file::{ReadError, read};
use crate::record::{Class, TrialFact};

/// The replay facts of `facts`: decisive trials recorded under a BIOS context. Idle failures never answer a
/// trial, and a fact without a context cannot say which machine it came from.
pub fn replay_facts(facts: &[TrialFact]) -> Vec<ReplayFact> {
    facts
        .iter()
        .filter_map(|fact| {
            let Class::Trial(class) = fact.class else {
                return None;
            };
            let context = fact.context.clone()?;
            let measured = Duration::from_secs(u64::from(fact.duration_s));
            Some(ReplayFact {
                context,
                regime: class.workload.regime(),
                workload: class.workload.as_str().to_owned(),
                cores: class.loaded,
                duration: Duration::from_secs(u64::from(class.duration_s)),
                profile: fact.profile.as_slice().to_vec(),
                outcome: match fact.outcome {
                    Decisive::Pass => ReplayOutcome::Pass,
                    Decisive::Failure { signal, .. } => ReplayOutcome::Failure {
                        signal,
                        exposure: measured,
                    },
                },
            })
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractsError {
    #[error("machine has no facts extract")]
    NoFacts,
    #[error("replay requires the machine to declare its BIOS context")]
    NoContext,
    #[error("load replay oracle: {0}")]
    Read(#[from] ReadError),
    #[error("load replay oracle: {0}")]
    Invalid(#[from] InvalidFact),
}

/// Extracts read so far, each once, keyed by the path a machine file resolves its `facts` to. The key is the
/// joined path as written, not a canonical one, so two spellings of one file are read twice.
#[derive(Debug, Default)]
pub struct Extracts {
    read: HashMap<PathBuf, Arc<[TrialFact]>>,
}

impl Extracts {
    pub fn new() -> Self {
        Self::default()
    }

    /// The path and records of the machine's extract, which resolves relative to the machine file.
    pub fn load(
        &mut self,
        machine_path: &Path,
        machine: &MachineDefinition,
    ) -> Result<(PathBuf, Arc<[TrialFact]>), ExtractsError> {
        let facts = machine
            .facts
            .as_deref()
            .filter(|facts| !facts.is_empty())
            .ok_or(ExtractsError::NoFacts)?;
        let path = machine_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(facts);
        if let Some(records) = self.read.get(&path) {
            return Ok((path, Arc::clone(records)));
        }
        let records: Arc<[TrialFact]> = read(&path)?.into();
        self.read.insert(path.clone(), Arc::clone(&records));
        Ok((path, records))
    }

    pub fn replay(
        &mut self,
        machine_path: &Path,
        machine: &MachineDefinition,
    ) -> Result<Replay, ExtractsError> {
        if machine.bios_context.is_none() {
            return Err(ExtractsError::NoContext);
        }
        let (_, records) = self.load(machine_path, machine)?;
        Ok(Replay::new(replay_facts(&records))?)
    }
}
