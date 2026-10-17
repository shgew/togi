//! The one place an offset reaches the SMU.
//!
//! A write needs a [`CheckedWrite`], which only [`Hardware`] builds, after the constraint check of the profile the
//! write would leave on the machine. [`apply`] spends it: the intent is made durable first and yields a receipt, the
//! seam write is derived from that receipt, the receipt is spent on the `smu.write`, and each affected core is read
//! back into the journal. Only a readback that agrees makes a slot [`Slot::Known`], and the [`Verified`] it returns
//! is what a trial needs to start.
//!
//! [`Verified`] cannot be built outside this module and [`apply`] takes the proof that no workload runs.

use togi_journal::{Payload, ProfileApplied, ProfileRestored, Seq, SmuCommand, SmuOp, SmuReadback};
use togi_machine::{
    Condition, CoreId, CoreSet, Error, ObservedOffset, PerCore, Smu, Stopped, TargetOffset,
};

use crate::appender::Appender;
use crate::error::{AppendFailure, JournalFailure};

/// The two authority boundaries of a hardware write, in one file with no dependency on the rest of the crate so
/// that the doctests below compile it whole and each negative case fails on the missing authority itself, not on a
/// private module: the SMU write seam needs a [`togi_journal::SmuIntentReceipt`], which only the journal issues
/// once the intent is durable, and a trial start needs an `Authorized` from a `Verified`, which only [`apply`] and
/// [`apply_profile`] build.
///
/// A command cannot reach the seam without the receipt of its durable intent:
///
/// ```compile_fail,E0308
/// mod authority { include!("write/authority.rs"); }
/// fn write(smu: &mut dyn togi_machine::Smu, command: togi_journal::SmuCommand) {
///     let _ = authority::execute(smu, command);
/// }
/// ```
///
/// The same call with the receipt compiles:
///
/// ```
/// mod authority { include!("write/authority.rs"); }
/// fn write(smu: &mut dyn togi_machine::Smu, receipt: &togi_journal::SmuIntentReceipt) {
///     let _ = authority::execute(smu, receipt);
/// }
/// ```
///
/// A trial cannot start without an authorization, nor from the verified result itself unauthorized:
///
/// ```compile_fail,E0308
/// mod authority { include!("write/authority.rs"); }
/// fn start(
///     trials: &mut dyn togi_machine::Trials,
///     cancel: &togi_machine::CancellationToken,
///     spec: togi_machine::TrialSpec,
///     ready: togi_machine::Stopped,
/// ) {
///     let _ = authority::start(trials, cancel, (), spec, ready);
/// }
/// ```
///
/// ```compile_fail,E0308
/// mod authority { include!("write/authority.rs"); }
/// fn start(
///     trials: &mut dyn togi_machine::Trials,
///     cancel: &togi_machine::CancellationToken,
///     verified: &authority::Verified,
///     spec: togi_machine::TrialSpec,
///     ready: togi_machine::Stopped,
/// ) {
///     let _ = authority::start(trials, cancel, verified, spec, ready);
/// }
/// ```
///
/// ```
/// mod authority { include!("write/authority.rs"); }
/// fn start(
///     trials: &mut dyn togi_machine::Trials,
///     cancel: &togi_machine::CancellationToken,
///     verified: &authority::Verified,
///     spec: togi_machine::TrialSpec,
///     ready: togi_machine::Stopped,
/// ) {
///     let _ = authority::start(trials, cancel, authority::authorize(verified), spec, ready);
/// }
/// ```
///
/// Neither proof can be forged:
///
/// ```compile_fail,E0451
/// mod authority { include!("write/authority.rs"); }
/// fn forge() -> authority::Authorized<'static> {
///     authority::Authorized { readbacks: &[] }
/// }
/// ```
///
/// ```compile_fail,E0451
/// mod authority { include!("write/authority.rs"); }
/// fn forge() -> authority::Verified {
///     authority::Verified { readbacks: Vec::new(), targets: Vec::new(), profile: None }
/// }
/// ```
pub(crate) mod authority;

pub(crate) use authority::Verified;
use authority::execute;

/// What prevents the constraint check from accepting the profile a write would leave.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConstraintRefusal {
    #[error("the tuner state holds no standing topology")]
    NoTopology,
    #[error("check the profile's cores")]
    Profile(#[from] togi_tuner::ProfileError),
    #[error("{0}")]
    Reached(togi_tuner::Reached),
    #[error("check the profile against the constraints' topology")]
    Topology(#[from] togi_tuner::TopologyMismatch),
}

/// The constraint check of `check_set`: refuses a profile that reaches a failure point or combination the tuner
/// state holds, one the state cannot judge, or cores other than the topology's. Never reads such a profile as safe.
#[cfg(test)]
pub(crate) fn refusal(
    state: &togi_tuner::State,
    after: &PerCore<TargetOffset>,
) -> Option<ConstraintRefusal> {
    judge(state.topology(), state.constraints(), after)
}

/// [`refusal`] against the tuner state as it stands now, for a write sequence that appends to the journal the state
/// is folded from.
pub(crate) fn checker(
    state: &togi_tuner::State,
) -> impl FnMut(&PerCore<TargetOffset>) -> Option<ConstraintRefusal> + use<> {
    let topology = state.topology().cloned();
    let constraints = state.constraints().cloned();
    move |after| judge(topology.as_ref(), constraints.as_ref(), after)
}

fn judge(
    topology: Option<&togi_tuner::Topology>,
    constraints: Option<&togi_tuner::Constraints>,
    after: &PerCore<TargetOffset>,
) -> Option<ConstraintRefusal> {
    let (Some(topology), Some(constraints)) = (topology, constraints) else {
        return Some(ConstraintRefusal::NoTopology);
    };
    let profile = match togi_tuner::Profile::new(topology, *after) {
        Ok(profile) => profile,
        Err(error) => return Some(ConstraintRefusal::Profile(error)),
    };
    match constraints.reaches(&profile) {
        Ok(reached) => reached.map(ConstraintRefusal::Reached),
        Err(mismatch) => Some(ConstraintRefusal::Topology(mismatch)),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Refused {
    #[error("refusing to write core {core:02} to {}: check the resulting profile", .target.get())]
    Reaches {
        core: CoreId,
        target: TargetOffset,
        #[source]
        refusal: ConstraintRefusal,
    },
    #[error("refusing to write core {core:02}: the offsets of the other cores are not known")]
    Unknown { core: CoreId },
    #[error("core {core:02} is not on this machine")]
    NoSuchCore { core: CoreId },
}

/// What a run knows of one core's offset.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Slot {
    /// Not known: never read, or a command went out and its readback did not confirm the target.
    #[default]
    Unknown,
    /// A readback confirmed this target.
    Known(TargetOffset),
}

/// The offsets this run knows the machine holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Hardware {
    cores: CoreSet,
    slots: PerCore<Slot>,
    engaged: bool,
}

impl Hardware {
    pub(crate) fn new(cores: CoreSet) -> Self {
        Self {
            cores,
            slots: cores.iter().map(|core| (core, Slot::Unknown)).collect(),
            engaged: false,
        }
    }

    pub(crate) const fn cores(&self) -> CoreSet {
        self.cores
    }

    pub(crate) fn slot(&self, core: CoreId) -> Option<Slot> {
        self.slots.get(core)
    }

    pub(crate) const fn engaged(&self) -> bool {
        self.engaged
    }

    fn observe(&mut self, core: CoreId, observed: ObservedOffset) {
        self.engaged = true;
        let slot = TargetOffset::new(observed.0).map_or(Slot::Unknown, Slot::Known);
        self.slots.set(core, slot);
    }

    /// The profile on the machine, when every core's offset is known.
    pub(crate) fn profile(&self) -> Option<PerCore<TargetOffset>> {
        self.cores
            .iter()
            .map(|core| match self.slots.get(core) {
                Some(Slot::Known(offset)) => Some((core, offset)),
                Some(Slot::Unknown) | None => None,
            })
            .collect()
    }

    /// The offsets known for the cores that have one, which a trial's spec conveys so the trial's own threads
    /// can stay off the cores held deepest. A core whose offset is not known is left out.
    pub(crate) fn confirmed(&self) -> PerCore<TargetOffset> {
        self.cores
            .iter()
            .filter_map(|core| match self.slots.get(core) {
                Some(Slot::Known(offset)) => Some((core, offset)),
                Some(Slot::Unknown) | None => None,
            })
            .collect()
    }

    /// A write of `target` to `core`. When it can deepen the core, `check` sees the profile the machine would hold
    /// afterwards and names what it reaches; a write that leaves the core as deep or shallower than it is known to
    /// be, or sets it to 0, moves toward no constraint and is not checked.
    pub(crate) fn check_set(
        &mut self,
        core: CoreId,
        target: TargetOffset,
        check: &mut dyn FnMut(&PerCore<TargetOffset>) -> Option<ConstraintRefusal>,
    ) -> Result<CheckedWrite<'_>, Refused> {
        if !self.cores.contains(core) {
            return Err(Refused::NoSuchCore { core });
        }
        let deepens = match self.slots.get(core) {
            Some(Slot::Known(held)) => target < held,
            Some(Slot::Unknown) | None => true,
        };
        if target.get() < 0 && deepens {
            let mut after = self
                .profile_with_unknown(core)
                .ok_or(Refused::Unknown { core })?;
            after.set(core, target);
            if let Some(refusal) = check(&after) {
                return Err(Refused::Reaches {
                    core,
                    target,
                    refusal,
                });
            }
        }
        self.engaged = true;
        Ok(CheckedWrite {
            hardware: self,
            command: SmuCommand::Set {
                core,
                offset: target,
            },
        })
    }

    /// A write of 0 to every core. Nothing to check: 0 reaches no failure point or combination.
    pub(crate) fn zero_all(&mut self) -> CheckedWrite<'_> {
        self.engaged = true;
        CheckedWrite {
            hardware: self,
            command: SmuCommand::SetAll {
                offset: TargetOffset::ZERO,
            },
        }
    }

    fn profile_with_unknown(&self, unknown: CoreId) -> Option<PerCore<TargetOffset>> {
        self.cores
            .iter()
            .map(|core| match self.slots.get(core) {
                Some(Slot::Known(offset)) => Some((core, offset)),
                _ if core == unknown => Some((core, TargetOffset::ZERO)),
                _ => None,
            })
            .collect()
    }
}

/// A write that passed the constraint check, borrowing the hardware it will change.
#[must_use = "a checked write does nothing until it is applied"]
#[derive(Debug)]
pub(crate) struct CheckedWrite<'h> {
    hardware: &'h mut Hardware,
    command: SmuCommand,
}

#[derive(Debug, thiserror::Error)]
pub enum WriteFailure {
    /// An append failed: the journal accepts nothing more, so the session answers with the emergency zeroing. After
    /// a failed `smu.write` there is no readback.
    #[error(transparent)]
    Journal(JournalFailure),
    /// The event is durable but the run could not absorb it.
    #[error(transparent)]
    Record(AppendFailure),
    /// The seam failed after the intent was durable; `smu.error` records it. A dead end.
    #[error("SMU {op} failed")]
    Smu {
        op: SmuOp,
        core: Option<CoreId>,
        #[source]
        error: Error,
    },
    /// A core read back something other than the target. The readback is durable. A dead end.
    #[error("core {core:02} reads CO {} after writing {}", .observed.0, .expected.get())]
    Mismatch {
        core: CoreId,
        observed: ObservedOffset,
        expected: TargetOffset,
    },
    #[error("the machine crashed during the write")]
    Crashed,
}

impl From<AppendFailure> for WriteFailure {
    fn from(failure: AppendFailure) -> Self {
        match failure {
            AppendFailure::Journal(failure) => Self::Journal(failure),
            other @ (AppendFailure::Fold(_) | AppendFailure::Boot(_)) => Self::Record(other),
        }
    }
}

/// Runs one checked write: intent, command, `smu.write`, then a readback of every core it touched.
///
/// The slots of those cores are unknown from before the command goes out until a readback confirms the target, so
/// a failure at any point leaves nothing believed that was not read.
pub(crate) fn apply(
    appender: &mut Appender,
    smu: &mut dyn Smu,
    _stopped: &Stopped,
    checked: CheckedWrite<'_>,
    causes: &[Seq],
) -> Result<Verified, WriteFailure> {
    let CheckedWrite { hardware, command } = checked;
    let target = command.offset();
    let touched: Vec<CoreId> = match command.core() {
        Some(core) => vec![core],
        None => hardware.cores.iter().collect(),
    };
    let receipt = appender.smu_intent(command, causes)?;
    let intent = receipt.intent();
    let command = receipt.command();
    for &core in &touched {
        hardware.slots.set(core, Slot::Unknown);
    }
    if let Err(error) = execute(smu, &receipt) {
        return Err(failed(
            appender,
            command.op(),
            command.core(),
            Some(target),
            &[intent],
            error,
        ));
    }
    let written = appender.smu_written(receipt)?;
    let mut readbacks = Vec::with_capacity(touched.len());
    for &core in &touched {
        let observed = match smu.offset(core) {
            Ok(observed) => observed,
            Err(error) => {
                return Err(failed(
                    appender,
                    SmuOp::Read,
                    Some(core),
                    Some(target),
                    &[written.seq()],
                    error,
                ));
            }
        };
        let seq = appender.append(
            Payload::SmuReadback(SmuReadback {
                core,
                offset: observed,
                expected: Some(target),
            }),
            &[written.seq()],
        )?;
        if observed != target.observed() {
            return Err(WriteFailure::Mismatch {
                core,
                observed,
                expected: target,
            });
        }
        hardware.slots.set(core, Slot::Known(target));
        readbacks.push(seq);
    }
    Ok(Verified::new(
        readbacks,
        touched.into_iter().map(|core| (core, target)).collect(),
        None,
    ))
}

fn failed(
    appender: &mut Appender,
    op: SmuOp,
    core: Option<CoreId>,
    offset: Option<TargetOffset>,
    causes: &[Seq],
    error: Error,
) -> WriteFailure {
    if matches!(error, Error::Crashed) {
        return WriteFailure::Crashed;
    }
    let record = Payload::SmuError(togi_journal::SmuError {
        op,
        core,
        offset,
        error: togi_render::error_chain(&error),
    });
    match appender.append(record, causes) {
        Ok(_) => WriteFailure::Smu { op, core, error },
        Err(failure) => failure.into(),
    }
}

/// What a read of every core's offset found, and the `smu.readback` events that recorded it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Reading {
    pub offsets: PerCore<ObservedOffset>,
    pub readbacks: Vec<Seq>,
}

/// Reads every core's offset, journals each as a readback without an expected value and lets the hardware know what
/// it found: a value no write could have put there leaves the slot unknown. A read that fails records `smu.error`.
pub(crate) fn read_all(
    appender: &mut Appender,
    smu: &mut dyn Smu,
    hardware: &mut Hardware,
    causes: &[Seq],
) -> Result<Reading, WriteFailure> {
    let mut offsets = PerCore::default();
    let mut readbacks = Vec::with_capacity(hardware.cores.len());
    for core in &hardware.cores {
        let observed = match smu.offset(core) {
            Ok(observed) => observed,
            Err(error) => {
                hardware.engaged = true;
                hardware.slots.set(core, Slot::Unknown);
                return Err(failed(
                    appender,
                    SmuOp::Read,
                    Some(core),
                    None,
                    causes,
                    error,
                ));
            }
        };
        hardware.observe(core, observed);
        let seq = appender.append(
            Payload::SmuReadback(SmuReadback {
                core,
                offset: observed,
                expected: None,
            }),
            causes,
        )?;
        offsets.set(core, observed);
        readbacks.push(seq);
    }
    Ok(Reading { offsets, readbacks })
}

/// What records a profile transaction once its readbacks agree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProfileRecord {
    Applied(Condition),
    Restored,
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileFailure {
    #[error(transparent)]
    Refused(#[from] Refused),
    #[error("a profile of {profile} offsets for a machine of {cores} cores")]
    Shape { profile: usize, cores: usize },
    #[error(transparent)]
    Write(#[from] WriteFailure),
}

impl From<AppendFailure> for ProfileFailure {
    fn from(failure: AppendFailure) -> Self {
        Self::Write(failure.into())
    }
}

/// A profile to put on the machine: its offsets, the events its writes cite, and what records it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProfileRequest<'a> {
    pub target: &'a PerCore<TargetOffset>,
    pub causes: &'a [Seq],
    pub record: ProfileRecord,
}

/// Puts a profile on the machine. When the profile on it is not known, every core is zeroed first. Then the cores
/// that get shallower are written in core order, then the cores that get deeper, each after the check of the
/// profile that step leaves. A step that fails leaves the steps before it written and read back and records no
/// profile.
pub(crate) fn apply_profile(
    appender: &mut Appender,
    smu: &mut dyn Smu,
    stopped: &Stopped,
    hardware: &mut Hardware,
    request: ProfileRequest<'_>,
    check: &mut dyn FnMut(&PerCore<TargetOffset>) -> Option<ConstraintRefusal>,
) -> Result<Verified, ProfileFailure> {
    let ProfileRequest {
        target,
        causes,
        record,
    } = request;
    let named: CoreSet = target.iter().map(|(core, _)| core).collect();
    if named != hardware.cores {
        return Err(ProfileFailure::Shape {
            profile: target.len(),
            cores: hardware.cores.len(),
        });
    }
    let mut readbacks = Vec::new();
    if hardware.profile().is_none() {
        let zeroed = apply(appender, smu, stopped, hardware.zero_all(), causes)?;
        readbacks.extend_from_slice(zeroed.readbacks());
    }
    for deeper in [false, true] {
        for (core, wanted) in target.iter() {
            let Some(Slot::Known(held)) = hardware.slot(core) else {
                return Err(Refused::Unknown { core }.into());
            };
            if wanted == held || (wanted < held) != deeper {
                continue;
            }
            let checked = hardware.check_set(core, wanted, check)?;
            let step = apply(appender, smu, stopped, checked, causes)?;
            readbacks.extend_from_slice(step.readbacks());
        }
    }
    let payload = match record {
        ProfileRecord::Applied(condition) => Payload::ProfileApplied(ProfileApplied {
            offsets: *target,
            condition,
        }),
        ProfileRecord::Restored => Payload::ProfileRestored(ProfileRestored { offsets: *target }),
    };
    let profile = appender.append(payload, &readbacks)?;
    Ok(Verified::new(
        readbacks,
        target.iter().collect(),
        Some(profile),
    ))
}

#[cfg(test)]
mod tests;
