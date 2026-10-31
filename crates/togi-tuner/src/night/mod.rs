//! Night-side policy of Ruleset 13: the coverage matrix, the night edge U with its mapping cursor,
//! the adequate-exposure predicate and the night scheduler. Pure: no clock, I/O or journal writes.

mod edge;
mod exposure;
mod matrix;
mod reproduction;
mod schedule;

pub use edge::{
    BRACKET_STEP, COARSE_STEP, Cursor, EdgeU, IsolatedObservation, IsolatedResult, Location,
    MappingBoard, MappingKind, MappingTarget, StepResult, UnitMapping, write_limit,
};
pub use exposure::{Conclusion, Credit, Launch, PlacementValidity, adequately_exposed};
pub use matrix::{
    Attempt, AttemptScope, Cell, HostPool, IDLE_DURATION, Layout, Matrix, MatrixError, PackageLoad,
    PlannedCell, Reuse, Roles, SEARCH_DURATION, SHORT_DURATION, SUSTAINED_DURATION, Source,
    Uncovered, Workloads, mapping_cells, minimum_position_seconds, qualification_cells,
    reproduction_cells,
};
pub use reproduction::{AttemptLog, AttemptNight, reproduction_stage};
pub use schedule::{
    Budgets, Consumed, Inputs, MAPPING_SHARE_PCT, NightTask, QUALIFICATION_SHARE_PCT, Queue,
    REPRODUCTION_SHARE_PCT, REQUALIFICATION_SHARE_PCT, Reason, Reproduction, ReproductionStage,
    RequalificationDue, WallSecs, Work, next_task,
};
pub use togi_machine::Purpose;
