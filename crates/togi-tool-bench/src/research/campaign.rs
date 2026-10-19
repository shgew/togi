use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;

use super::command::{self, CommandError};
use super::ledger::{Decision, End, Ended, Ledger, Outcome, Record, Role, Started, StopReason};
use super::manifest::{Issue, Manifest, ManifestError, Mode, sha256};
use super::{Workspace, WorkspaceError};

/// What ends an action: a refusal records nothing; a stop appends `campaign_stopped` and ends the campaign.
#[derive(Debug, thiserror::Error)]
pub enum Halt {
    #[error(transparent)]
    Refuse(#[from] Refusal),
    #[error("campaign stopped: {}: {detail}", reason.name())]
    Stop { reason: StopReason, detail: String },
}

#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("read the tree")]
    Workspace(#[from] WorkspaceError),
    #[error("read elapsed campaign time")]
    Elapsed(#[source] jiff::Error),
    #[error("the campaign already stopped: {}", .0.name())]
    Stopped(StopReason),
    #[error("runs {0:?} never ended; run `resume` to record them incomplete first")]
    Dangling(Vec<u32>),
    #[error("the run directory's manifest is not the one the campaign started with")]
    ManifestReplaced,
    #[error("HEAD {head} is not the manifest's checkout {checkout}")]
    NotAtCheckout { head: String, checkout: String },
    #[error("the tree differs from the checkout at {0:?}; a campaign starts from a clean checkout")]
    Dirty(Vec<String>),
    #[error("{0} is not in the manifest's hypothesis queue")]
    UnknownCandidate(String),
    #[error("{0}")]
    Order(String),
    #[error("the outcome needs metrics read from the runs' output")]
    NoMetrics,
    #[error(
        "the output directory {0} is inside the checkout; a campaign's record stays outside what it commits"
    )]
    OutputInCheckout(String),
}

fn stop(reason: StopReason, detail: impl Into<String>) -> Halt {
    Halt::Stop {
        reason,
        detail: detail.into(),
    }
}

/// A campaign's manifest and ledger, and the rules every action checks against them.
#[derive(Clone, Debug)]
pub struct Campaign {
    pub manifest: Manifest,
    pub issue: Issue,
    pub ledger: Ledger,
}

/// A run the dispatcher may now start: its start record and its time budget.
#[derive(Clone, Debug)]
pub struct Planned {
    pub started: Started,
    pub budget: Duration,
}

/// Validates a manifest against the tree and returns the campaign's first record.
pub fn start(
    manifest_bytes: &[u8],
    issue: u32,
    workspace: &dyn Workspace,
    now: Timestamp,
) -> Result<(Manifest, Record), Refusal> {
    let manifest = Manifest::parse(manifest_bytes)?;
    manifest.validate(issue)?;
    if Path::new(&manifest.output).starts_with(workspace.root()) {
        return Err(Refusal::OutputInCheckout(manifest.output));
    }
    let head = workspace.head()?;
    if head != manifest.checkout {
        return Err(Refusal::NotAtCheckout {
            head,
            checkout: manifest.checkout,
        });
    }
    let changed = workspace.changed(&manifest.checkout)?;
    if !changed.is_empty() {
        return Err(Refusal::Dirty(changed.into_iter().collect()));
    }
    if !workspace.is_ancestor(&manifest.reference, &manifest.checkout)? {
        return Err(Refusal::Order(format!(
            "the reference {} is not an ancestor of the checkout",
            manifest.reference
        )));
    }
    let unwritable: Vec<String> = workspace
        .changed(&manifest.reference)?
        .into_iter()
        .filter(|path| !manifest.may_write(path))
        .collect();
    if !unwritable.is_empty() {
        return Err(Refusal::Order(format!(
            "the reference differs from the checkout outside the writable paths, at {}; no reference run could pass the checks",
            unwritable.join(", ")
        )));
    }
    manifest.check_tree(workspace)?;
    let record = Record::CampaignStarted {
        at: now,
        manifest_sha256: sha256(manifest_bytes),
        head,
    };
    Ok((manifest, record))
}

impl Campaign {
    /// Reads a run directory's manifest and ledger, refusing a manifest that is not the one the ledger started with.
    pub fn open(manifest_bytes: &[u8], issue: u32, ledger: Ledger) -> Result<Self, Refusal> {
        if ledger.manifest_sha256() != Some(sha256(manifest_bytes).as_str()) {
            return Err(Refusal::ManifestReplaced);
        }
        let manifest = Manifest::parse(manifest_bytes)?;
        let issue = manifest.validate(issue)?;
        Ok(Self {
            manifest,
            issue,
            ledger,
        })
    }

    /// Records every run found unfinished as incomplete, and the resumption.
    pub fn resume(&self, workspace: &dyn Workspace, now: Timestamp) -> Result<Vec<Record>, Halt> {
        if let Some((reason, _)) = self.ledger.stopped() {
            return Err(Refusal::Stopped(reason).into());
        }
        let mut records: Vec<Record> = self
            .ledger
            .dangling()
            .into_iter()
            .map(|seq| {
                Record::ExperimentEnded(Ended {
                    seq,
                    at: now,
                    end: End::Incomplete,
                    stdout_sha256: None,
                    stderr_sha256: None,
                })
            })
            .collect();
        records.push(Record::CampaignResumed {
            at: now,
            head: workspace.head().map_err(Refusal::from)?,
        });
        Ok(records)
    }

    /// The checks every action makes before it records anything: the campaign is live, nothing is left
    /// unfinished, the wall time remains, the pinned files are unchanged and the tree changed only where the
    /// brief allows. Returns the wall time left.
    pub fn guard(&self, workspace: &dyn Workspace, now: Timestamp) -> Result<Duration, Halt> {
        if let Some((reason, _)) = self.ledger.stopped() {
            return Err(Refusal::Stopped(reason).into());
        }
        let dangling = self.ledger.dangling();
        if !dangling.is_empty() {
            return Err(Refusal::Dangling(dangling).into());
        }
        let left = self.wall_left(now)?;
        if left.is_zero() {
            return Err(stop(
                StopReason::WallLimit,
                format!("{} minutes used", self.manifest.limits.wall_minutes),
            ));
        }
        if let Err(error) = self.manifest.check_tree(workspace) {
            let mut detail = error.to_string();
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                detail = format!("{detail}: {cause}");
                source = cause.source();
            }
            return Err(stop(StopReason::EvidenceChanged, detail));
        }
        let head = workspace.head().map_err(Refusal::from)?;
        if !workspace
            .is_ancestor(&self.manifest.checkout, &head)
            .map_err(Refusal::from)?
        {
            return Err(stop(
                StopReason::ForbiddenDiff,
                format!("HEAD {head} does not descend from the checkout"),
            ));
        }
        let forbidden: Vec<String> = workspace
            .changed(&self.manifest.checkout)
            .map_err(Refusal::from)?
            .into_iter()
            .filter(|path| !self.manifest.may_write(path))
            .collect();
        if !forbidden.is_empty() {
            return Err(stop(
                StopReason::ForbiddenDiff,
                format!(
                    "changed outside the writable paths: {}",
                    forbidden.join(", ")
                ),
            ));
        }
        Ok(left)
    }

    fn wall_left(&self, now: Timestamp) -> Result<Duration, Refusal> {
        let limit = Duration::from_secs(u64::from(self.manifest.limits.wall_minutes) * 60);
        let Some(started) = self.ledger.started_at() else {
            return Ok(limit);
        };
        let used = Duration::try_from(now.duration_since(started)).map_err(Refusal::Elapsed)?;
        Ok(limit.saturating_sub(used))
    }

    /// Checks a run against the guard, the queue, the limits, the role's order and the evaluator's allowlist,
    /// and returns its start record.
    pub fn plan(
        &self,
        workspace: &dyn Workspace,
        now: Timestamp,
        candidate: &str,
        role: Role,
        recipe: &str,
        args: &[String],
    ) -> Result<Planned, Halt> {
        let budget = self.guard(workspace, now)?;
        if !self
            .manifest
            .hypotheses
            .iter()
            .any(|hypothesis| hypothesis.id == candidate)
        {
            return Err(Refusal::UnknownCandidate(candidate.to_owned()).into());
        }
        let recipe = command::recipe(self.manifest.mode, recipe).map_err(Refusal::from)?;
        let args = command::arguments(&self.manifest, recipe, role, args).map_err(Refusal::from)?;
        let tree = self.tree_identity(workspace)?;
        let order = |message: String| Halt::from(Refusal::Order(message));
        match role {
            Role::Reference => {
                let changed = workspace
                    .changed(&self.manifest.reference)
                    .map_err(Refusal::from)?;
                if !changed.is_empty() {
                    return Err(order(format!(
                        "a reference run needs the tree at the reference; it differs at {}",
                        changed.into_iter().collect::<Vec<_>>().join(", ")
                    )));
                }
            }
            Role::Candidate => {
                let candidates = self.ledger.candidates();
                if !candidates.contains(&candidate)
                    && candidates.len() >= self.manifest.limits.candidates as usize
                {
                    return Err(stop(
                        StopReason::CandidateLimit,
                        format!("{} candidates evaluated", candidates.len()),
                    ));
                }
                if self.ledger.outcome(candidate).is_some() {
                    return Err(order(format!("{candidate} already has an outcome")));
                }
            }
            Role::Confirmation => {
                let spent = self.ledger.confirmations();
                if spent.iter().any(|started| started.candidate == candidate) {
                    return Err(order(format!(
                        "{candidate}'s confirmation is already spent"
                    )));
                }
                if spent.len() >= self.manifest.confirmations as usize {
                    return Err(order(format!(
                        "all {} confirmations are spent",
                        self.manifest.confirmations
                    )));
                }
                let kept = self.kept_run(candidate)?;
                if kept.tree_sha256 != tree {
                    return Err(order(format!(
                        "the tree is not the kept candidate {candidate} (run {}); a confirmation scores exactly that candidate",
                        kept.seq
                    )));
                }
            }
            Role::ConfirmationReference => {
                if !self
                    .ledger
                    .confirmations()
                    .iter()
                    .any(|started| started.candidate == candidate)
                {
                    return Err(order(format!(
                        "{candidate}'s confirmation has not started; its reference runs after it"
                    )));
                }
                let changed = workspace
                    .changed(&self.manifest.reference)
                    .map_err(Refusal::from)?;
                if !changed.is_empty() {
                    return Err(order(
                        "a reference run needs the tree at the reference".to_owned(),
                    ));
                }
            }
            Role::Finalization => {
                let passed = self
                    .ledger
                    .outcome(candidate)
                    .is_some_and(|decision| decision.outcome == Outcome::ConfirmationPassed);
                let confirmed = self.ledger.latest_complete(candidate, Role::Confirmation);
                match confirmed {
                    Some(run) if passed && run.tree_sha256 == tree => {}
                    _ => {
                        return Err(order(format!(
                            "finalization checks a confirmed candidate; {candidate} is not confirmed at this tree"
                        )));
                    }
                }
            }
        }
        let seq = self.ledger.next_seq();
        let stem = format!("raw/{seq:04}-{candidate}-{}", role.name());
        Ok(Planned {
            started: Started {
                seq,
                at: now,
                candidate: candidate.to_owned(),
                role,
                recipe,
                args,
                head: workspace.head().map_err(Refusal::from)?,
                tree_sha256: tree,
                reference: self.manifest.reference.clone(),
                pins: self.manifest.pins(),
                stdout: format!("{stem}.stdout"),
                stderr: format!("{stem}.stderr"),
            },
            budget,
        })
    }

    fn kept_run(&self, candidate: &str) -> Result<&Started, Halt> {
        let kept = self
            .ledger
            .outcome(candidate)
            .filter(|decision| decision.outcome == Outcome::Kept);
        let run = kept.and_then(|_| self.ledger.latest_complete(candidate, Role::Candidate));
        run.ok_or_else(|| Refusal::Order(format!("{candidate} is not a kept candidate")).into())
    }

    /// The SHA-256 of every changed path and its content: the candidate's identity.
    fn tree_identity(&self, workspace: &dyn Workspace) -> Result<String, Halt> {
        let changed = workspace
            .changed(&self.manifest.checkout)
            .map_err(Refusal::from)?;
        let mut bytes = Vec::new();
        for path in changed {
            bytes.extend_from_slice(path.as_bytes());
            bytes.push(0);
            match workspace.read(&path) {
                Ok(content) => {
                    bytes.extend_from_slice(sha256(&content).as_bytes());
                }
                Err(WorkspaceError::Missing(_)) => bytes.extend_from_slice(b"deleted"),
                Err(error) => return Err(Refusal::from(error).into()),
            }
            bytes.push(0);
        }
        Ok(sha256(&bytes))
    }

    /// The end record of a run, and the stop it implies: a run stopped at the wall-time limit ends the campaign.
    pub fn finish(
        started: &Started,
        now: Timestamp,
        end: End,
        stdout_sha256: Option<String>,
        stderr_sha256: Option<String>,
    ) -> Vec<Record> {
        let mut records = vec![Record::ExperimentEnded(Ended {
            seq: started.seq,
            at: now,
            end,
            stdout_sha256,
            stderr_sha256,
        })];
        if end == End::TimedOut {
            records.push(Record::CampaignStopped {
                at: now,
                reason: StopReason::WallLimit,
                detail: format!("run {} stopped at the limit", started.seq),
            });
        }
        records
    }

    /// Records a candidate's outcome after checking that the runs it rests on are complete and paired. A failed
    /// confirmation also stops the campaign.
    pub fn decide(
        &self,
        workspace: &dyn Workspace,
        now: Timestamp,
        candidate: &str,
        outcome: Outcome,
        reason: &str,
        metrics: BTreeMap<String, f64>,
    ) -> Result<Vec<Record>, Halt> {
        self.guard(workspace, now)?;
        if !self
            .manifest
            .hypotheses
            .iter()
            .any(|hypothesis| hypothesis.id == candidate)
        {
            return Err(Refusal::UnknownCandidate(candidate.to_owned()).into());
        }
        if reason.trim().is_empty() {
            return Err(Refusal::Order("an outcome states its reason".to_owned()).into());
        }
        let previous = self
            .ledger
            .outcome(candidate)
            .map(|decision| decision.outcome);
        let runs = match outcome {
            Outcome::Kept | Outcome::Rejected | Outcome::Invalid => {
                if previous.is_some() {
                    return Err(
                        Refusal::Order(format!("{candidate} already has an outcome")).into(),
                    );
                }
                self.development_runs(candidate, outcome)?
            }
            Outcome::ConfirmationPassed | Outcome::ConfirmationFailed => {
                if previous != Some(Outcome::Kept) {
                    return Err(Refusal::Order(format!(
                        "{candidate} is not a kept candidate awaiting its confirmation"
                    ))
                    .into());
                }
                let Some(run) = self.ledger.latest_complete(candidate, Role::Confirmation) else {
                    return Err(stop(
                        StopReason::UnpairedComparison,
                        format!("{candidate}'s confirmation did not complete"),
                    ));
                };
                let Some(reference) = self.ledger.pair(run, Role::ConfirmationReference) else {
                    return Err(stop(
                        StopReason::UnpairedComparison,
                        format!(
                            "{candidate}'s confirmation has no complete reference run with the same arguments and data"
                        ),
                    ));
                };
                vec![reference.seq, run.seq]
            }
        };
        if metrics.is_empty() && outcome != Outcome::Invalid {
            return Err(Refusal::NoMetrics.into());
        }
        let mut records = vec![Record::Outcome(Decision {
            at: now,
            candidate: candidate.to_owned(),
            outcome,
            reason: reason.to_owned(),
            metrics,
            runs,
        })];
        if outcome == Outcome::ConfirmationFailed {
            records.push(Record::CampaignStopped {
                at: now,
                reason: StopReason::ConfirmationFailed,
                detail: format!("{candidate}: {reason}"),
            });
        }
        Ok(records)
    }

    /// The runs a development outcome rests on. A kept outcome needs the candidate's complete run, and in model
    /// development a complete reference run with the same evaluator, arguments and data.
    fn development_runs(&self, candidate: &str, outcome: Outcome) -> Result<Vec<u32>, Halt> {
        let run = self.ledger.latest_complete(candidate, Role::Candidate);
        if outcome != Outcome::Kept {
            let mut runs: Vec<u32> = self
                .ledger
                .starts()
                .filter(|started| started.candidate == candidate)
                .map(|started| started.seq)
                .collect();
            runs.sort_unstable();
            return Ok(runs);
        }
        let Some(run) = run else {
            return Err(stop(
                StopReason::UnpairedComparison,
                format!("{candidate} has no complete candidate run to keep"),
            ));
        };
        match self.manifest.mode {
            Mode::ModelDevelopment => match self.ledger.pair(run, Role::Reference) {
                Some(reference) => Ok(vec![reference.seq, run.seq]),
                None => Err(stop(
                    StopReason::UnpairedComparison,
                    format!(
                        "{candidate}'s run {} has no complete reference run with the same arguments and data",
                        run.seq
                    ),
                )),
            },
            Mode::FixedPolicyObservation | Mode::CounterfactualAccounting => Ok(vec![run.seq]),
        }
    }

    /// The researcher's stop: the queue is done, a prerequisite is missing, or the campaign was interrupted.
    pub fn stop(
        &self,
        now: Timestamp,
        reason: StopReason,
        detail: &str,
    ) -> Result<Record, Refusal> {
        if let Some((stopped, _)) = self.ledger.stopped() {
            return Err(Refusal::Stopped(stopped));
        }
        Ok(Record::CampaignStopped {
            at: now,
            reason,
            detail: detail.to_owned(),
        })
    }
}
