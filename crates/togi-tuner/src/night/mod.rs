//! Night-side policy of Ruleset 13: the coverage matrix, the night edge U with its mapping cursor,
//! the adequate-exposure predicate and the night scheduler. Pure: no clock, I/O or journal writes.

mod edge;
mod exposure;
mod matrix;
mod reproduction;
mod schedule;

pub use edge::{
    BRACKET_STEP, COARSE_STEP, CcdMapping, Cursor, EdgeU, IsolatedObservation, IsolatedResult,
    Location, MappingBoard, MappingKind, MappingTarget, StepResult, write_limit,
};
pub use exposure::{
    Conclusion, Credit, Launch, PlacementValidity, ROLE_WARM_UP_S, WARM_UP_S, adequately_exposed,
    warm_up_secs,
};
pub use matrix::{
    Attempt, AttemptScope, Cell, HostPool, IDLE_DURATION, Layout, Matrix, MatrixError, PackageLoad,
    PlannedCell, Reuse, Roles, SEARCH_DURATION, SHORT_DURATION, SUSTAINED_DURATION,
    Source, Uncovered, Workloads, control_cells, mapping_cells, minimum_position_seconds,
    qualification_cells, reproduction_cells, zeroed,
};
pub use reproduction::{AttemptLog, AttemptNight, reproduction_stage};
pub use schedule::{
    Budgets, CONTROLS_SHARE_PCT, Consumed, ControlCause, Inputs, MAPPING_SHARE_PCT, NightTask,
    QUALIFICATION_SHARE_PCT, Queue, REPRODUCTION_SHARE_PCT, REQUALIFICATION_SHARE_PCT, Reason,
    Reproduction, ReproductionStage, RequalificationDue, Stock, WallSecs, Work, next_task,
};
pub use togi_machine::Purpose;
