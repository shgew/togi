use serde::{Deserialize, Serialize};
use togi_machine::{CoreId, ObservedOffset, TargetOffset};

use super::domain::SmuOp;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SmuCommand {
    Set { core: CoreId, offset: TargetOffset },
    SetAll { offset: TargetOffset },
}

impl SmuCommand {
    pub const fn offset(&self) -> TargetOffset {
        match *self {
            Self::Set { offset, .. } | Self::SetAll { offset } => offset,
        }
    }

    pub const fn core(&self) -> Option<CoreId> {
        match *self {
            Self::Set { core, .. } => Some(core),
            Self::SetAll { .. } => None,
        }
    }

    pub const fn op(&self) -> SmuOp {
        match self {
            Self::Set { .. } => SmuOp::Set,
            Self::SetAll { .. } => SmuOp::SetAll,
        }
    }
}

/// The durable authorization of one SMU command, written only by the journal.
///
/// ```compile_fail,E0451
/// let command = togi_journal::SmuCommand::SetAll { offset: togi_machine::TargetOffset::ZERO };
/// let _ = togi_journal::SmuIntentRecord { command };
/// ```
///
/// ```compile_fail,E0624
/// let command = togi_journal::SmuCommand::SetAll { offset: togi_machine::TargetOffset::ZERO };
/// let _ = togi_journal::SmuIntentRecord::new(command);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SmuIntentRecord {
    command: SmuCommand,
}

impl SmuIntentRecord {
    #[cfg_attr(not(test), expect(dead_code, reason = "the journal's SMU append mints the record"))]
    pub(crate) const fn new(command: SmuCommand) -> Self {
        Self { command }
    }

    pub const fn command(&self) -> SmuCommand {
        self.command
    }
}

/// The result of one SMU command, written only by the journal.
///
/// ```compile_fail,E0451
/// let command = togi_journal::SmuCommand::SetAll { offset: togi_machine::TargetOffset::ZERO };
/// let _ = togi_journal::SmuWriteRecord { command };
/// ```
///
/// ```compile_fail,E0624
/// let command = togi_journal::SmuCommand::SetAll { offset: togi_machine::TargetOffset::ZERO };
/// let _ = togi_journal::SmuWriteRecord::new(command);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SmuWriteRecord {
    command: SmuCommand,
}

impl SmuWriteRecord {
    #[cfg_attr(not(test), expect(dead_code, reason = "the journal's SMU append mints the record"))]
    pub(crate) const fn new(command: SmuCommand) -> Self {
        Self { command }
    }

    pub const fn command(&self) -> SmuCommand {
        self.command
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmuReadback {
    pub core: CoreId,
    pub offset: ObservedOffset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<TargetOffset>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmuError {
    pub op: SmuOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<CoreId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<TargetOffset>,
    pub error: String,
}
