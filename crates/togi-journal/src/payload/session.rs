use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use togi_machine::{
    BiosContext, CoreId, CoreInfo, CoreSet, KernelCursor, ObservedOffset, PerCore, Signal,
    TargetOffset,
};

use super::domain::{
    DeadEndAction, DeadEndCondition, DefectAnswer, DefectDirection, Notice, ShutdownReason,
};
use super::trial::TrialId;
use crate::{Build, Seq, SessionId};

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes skipped fields by reference"
)]
pub(super) fn is_false(value: &bool) -> bool {
    !*value
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes skipped fields by reference"
)]
pub(super) fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStart {
    pub build: Build,
    pub session: SessionId,
    pub cores: Vec<CoreInfo>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionContext {
    pub bios_version: String,
    pub board: String,
    pub cpu_model: String,
    pub microcode: String,
    pub boost_limit_mhz: u32,
}

impl SessionContext {
    pub fn bios_context(&self) -> BiosContext {
        BiosContext {
            bios_version: self.bios_version.clone(),
            board: self.board.clone(),
            cpu_model: self.cpu_model.clone(),
            microcode: self.microcode.clone(),
            boost_limit_mhz: self.boost_limit_mhz,
        }
    }
}

impl From<BiosContext> for SessionContext {
    fn from(context: BiosContext) -> Self {
        Self {
            bios_version: context.bios_version,
            board: context.board,
            cpu_model: context.cpu_model,
            microcode: context.microcode,
            boost_limit_mhz: context.boost_limit_mhz,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionBaseline {
    pub offsets: PerCore<ObservedOffset>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionNotice {
    pub notice: Notice,
    #[serde(default, skip_serializing_if = "CoreSet::is_empty")]
    pub cores: CoreSet,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionWarning {
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<TrialId>,
    pub error: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionArchived {
    pub session: SessionId,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub session: SessionId,
    pub seq: Seq,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarriedLimit {
    pub offset: TargetOffset,
    pub from: Provenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarriedFailurePoint {
    pub offset: TargetOffset,
    pub from: Provenance,
    pub signal: Signal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarriedSource {
    pub session: SessionId,
    pub schema: u32,
    pub ruleset: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarriedCore {
    pub core: CoreId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_solo_limit: Option<CarriedLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_point: Option<CarriedFailurePoint>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCarried {
    pub sources: Vec<CarriedSource>,
    pub failure_points: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<CarriedCore>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigLoaded {
    pub build: Build,
    pub config: togi_config::Config,
    pub path: String,
    pub file: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_cursor: Option<KernelCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreflightCheck {
    pub check: String,
    pub detail: String,
    pub ok: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileApplied {
    pub offsets: PerCore<TargetOffset>,
    pub condition: togi_machine::Condition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<PerCore<TargetOffset>>,
    pub to: PerCore<TargetOffset>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRestored {
    pub offsets: PerCore<TargetOffset>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetTarget {
    Core(CoreId),
    All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandReset {
    pub target: ResetTarget,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefectFound {
    pub id: NonZeroU32,
    pub title: String,
    pub detail: String,
    pub pr: u32,
    pub direction: DefectDirection,
    pub cores: CoreSet,
    pub decisions: Vec<Seq>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefectAnswered {
    pub id: NonZeroU32,
    pub cores: CoreSet,
    pub answer: DefectAnswer,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeadEnd {
    pub condition: DeadEndCondition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<CoreId>,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<DeadEndAction>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootSavedEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootLeaveReason {
    pub reason_id: String,
    pub restart_limit_count: u8,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shutdown {
    pub reason: ShutdownReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_cursor: Option<KernelCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycles: Option<NonZeroU32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalTorn {
    pub offset: u64,
    pub bytes_hex: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateRebuilt {
    pub fields: Vec<String>,
}
