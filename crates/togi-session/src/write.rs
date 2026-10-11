//! The one place an offset reaches the SMU.
//!
//! A write needs a [`CheckedWrite`], which only [`Hardware`] builds, after the constraint check of the profile the
//! write would leave on the machine. [`apply`] spends it: the intent is made durable first and yields a receipt, the
//! seam write is derived from that receipt, the receipt is spent on the `smu.write`, and each affected core is read
//! back into the journal. Only a readback that agrees makes a slot [`Slot::Known`], and the [`Verified`] it returns
//! is what a trial needs to start.
//!
//! Neither [`CheckedWrite`] nor [`Verified`] can be built outside this module:
//!
//! ```compile_fail,E0451
//! fn forge(hardware: &mut togi_session::write::Hardware) -> togi_session::write::CheckedWrite<'_> {
//!     togi_session::write::CheckedWrite { hardware, command: todo!() }
//! }
//! ```
//!
//! ```compile_fail,E0451
//! fn forge() -> togi_session::write::Verified {
//!     togi_session::write::Verified { readbacks: Vec::new(), targets: Vec::new(), profile: None }
//! }
//! ```
//!
//! And [`apply`] takes the proof that no workload runs:
//!
//! ```compile_fail,E0061
//! fn without_proof(
//!     appender: &mut togi_session::appender::Appender,
//!     smu: &mut dyn togi_machine::Smu,
//!     checked: togi_session::write::CheckedWrite<'_>,
//! ) {
//!     let _ = togi_session::write::apply(appender, smu, checked, &[]);
//! }
//! ```

use togi_journal::{Payload, ProfileApplied, ProfileRestored, Seq, SmuCommand, SmuOp, SmuReadback};
use togi_machine::{
    Condition, CoreId, CoreSet, Error, ObservedOffset, PerCore, Smu, Stopped, TargetOffset,
};

use crate::appender::Appender;
use crate::error::{AppendFailure, JournalFailure};

/// What the profile a write would leave reaches: a failure point or a combination.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct Refusal(String);

impl Refusal {
    pub fn new(reached: impl Into<String>) -> Self {
        Self(reached.into())
    }
}

/// The constraint check of `check_set`: refuses a profile that reaches a failure point or combination the tuner
/// state holds, one the state cannot judge, or cores other than the topology's. Never reads such a profile as safe.
pub fn refusal(state: &togi_tuner::State, after: &PerCore<TargetOffset>) -> Option<Refusal> {
    let (Some(topology), Some(constraints)) = (state.topology(), state.constraints()) else {
        return Some(Refusal::new("the tuner state holds no standing topology"));
    };
    let profile = match togi_tuner::Profile::new(topology, *after) {
        Ok(profile) => profile,
        Err(error) => return Some(Refusal::new(error.to_string())),
    };
    match constraints.reaches(&profile) {
        Ok(reached) => reached.map(|reached| Refusal::new(reached.to_string())),
        Err(mismatch) => Some(Refusal::new(mismatch.to_string())),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Refused {
    #[error("refusing to write core {core:02} to {}: the profile would reach {refusal}", .target.get())]
    Reaches {
        core: CoreId,
        target: TargetOffset,
        refusal: Refusal,
    },
    #[error("refusing to write core {core:02}: the offsets of the other cores are not known")]
    Unknown { core: CoreId },
    #[error("core {core:02} is not on this machine")]
    NoSuchCore { core: CoreId },
}

/// What a run knows of one core's offset.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Slot {
    /// Not known: never read, or a command went out and its readback did not confirm the target.
    #[default]
    Unknown,
    /// A readback confirmed this target.
    Known(TargetOffset),
}

/// The offsets this run knows the machine holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hardware {
    cores: CoreSet,
    slots: PerCore<Slot>,
}

impl Hardware {
    pub fn new(cores: CoreSet) -> Self {
        Self {
            cores,
            slots: cores.iter().map(|core| (core, Slot::Unknown)).collect(),
        }
    }

    pub const fn cores(&self) -> CoreSet {
        self.cores
    }

    pub fn slot(&self, core: CoreId) -> Option<Slot> {
        self.slots.get(core)
    }

    /// The profile on the machine, when every core's offset is known.
    pub fn profile(&self) -> Option<PerCore<TargetOffset>> {
        self.cores
            .iter()
            .map(|core| match self.slots.get(core) {
                Some(Slot::Known(offset)) => Some((core, offset)),
                Some(Slot::Unknown) | None => None,
            })
            .collect()
    }

    /// A write of `target` to `core`. When it can deepen the core, `check` sees the profile the machine would hold
    /// afterwards and names what it reaches; a write that leaves the core as deep or shallower than it is known to
    /// be, or sets it to 0, moves toward no constraint and is not checked.
    pub fn check_set(
        &mut self,
        core: CoreId,
        target: TargetOffset,
        check: &mut dyn FnMut(&PerCore<TargetOffset>) -> Option<Refusal>,
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
        Ok(CheckedWrite {
            hardware: self,
            command: SmuCommand::Set {
                core,
                offset: target,
            },
        })
    }

    /// A write of 0 to every core. Nothing to check: 0 reaches no failure point or combination.
    pub fn zero_all(&mut self) -> CheckedWrite<'_> {
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
pub struct CheckedWrite<'h> {
    hardware: &'h mut Hardware,
    command: SmuCommand,
}

/// Proof that every command of a transaction reached the machine and its readbacks agreed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Verified {
    readbacks: Vec<Seq>,
    targets: Vec<(CoreId, TargetOffset)>,
    profile: Option<Seq>,
}

impl Verified {
    /// The `smu.readback` events that confirmed it, oldest first.
    pub fn readbacks(&self) -> &[Seq] {
        &self.readbacks
    }

    /// The offsets confirmed, by core.
    pub fn targets(&self) -> &[(CoreId, TargetOffset)] {
        &self.targets
    }

    /// The `profile.applied` or `profile.restored` that recorded a profile transaction.
    pub const fn profile(&self) -> Option<Seq> {
        self.profile
    }
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
    #[error("SMU {op} failed: {error}")]
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

#[expect(
    clippy::disallowed_methods,
    reason = "the write transaction is the only caller of the SMU write seam"
)]
fn execute(smu: &mut dyn Smu, command: SmuCommand) -> Result<(), Error> {
    match command {
        SmuCommand::Set { core, offset } => smu.set_offset(core, offset),
        SmuCommand::SetAll { offset } => smu.set_all_offsets(offset),
    }
}

/// Runs one checked write: intent, command, `smu.write`, then a readback of every core it touched.
///
/// The slots of those cores are unknown from before the command goes out until a readback confirms the target, so
/// a failure at any point leaves nothing believed that was not read.
pub fn apply(
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
    if let Err(error) = execute(smu, command) {
        return Err(failed(
            appender,
            command.op(),
            command.core(),
            target,
            intent,
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
                    target,
                    written.seq(),
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
    Ok(Verified {
        readbacks,
        targets: touched.into_iter().map(|core| (core, target)).collect(),
        profile: None,
    })
}

fn failed(
    appender: &mut Appender,
    op: SmuOp,
    core: Option<CoreId>,
    target: TargetOffset,
    cause: Seq,
    error: Error,
) -> WriteFailure {
    if matches!(error, Error::Crashed) {
        return WriteFailure::Crashed;
    }
    let record = Payload::SmuError(togi_journal::SmuError {
        op,
        core,
        offset: Some(target),
        error: error.to_string(),
    });
    match appender.append(record, &[cause]) {
        Ok(_) => WriteFailure::Smu { op, core, error },
        Err(failure) => failure.into(),
    }
}

/// What records a profile transaction once its readbacks agree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileRecord {
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
pub struct ProfileRequest<'a> {
    pub target: &'a PerCore<TargetOffset>,
    pub causes: &'a [Seq],
    pub record: ProfileRecord,
}

/// Puts a profile on the machine. When the profile on it is not known, every core is zeroed first. Then the cores
/// that get shallower are written in core order, then the cores that get deeper, each after the check of the
/// profile that step leaves. A step that fails leaves the steps before it written and read back and records no
/// profile.
pub fn apply_profile(
    appender: &mut Appender,
    smu: &mut dyn Smu,
    stopped: &Stopped,
    hardware: &mut Hardware,
    request: ProfileRequest<'_>,
    check: &mut dyn FnMut(&PerCore<TargetOffset>) -> Option<Refusal>,
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
    Ok(Verified {
        readbacks,
        targets: target.iter().collect(),
        profile: Some(profile),
    })
}

#[cfg(test)]
mod tests;
