//! Successor day/night registration, scoring and Markdown evidence, separate from Ruleset 13.
//!
//! This module consumes synthetic or recorded physical-core trajectories; it never runs a
//! simulation or inspects a policy's private state. The runner supplies policy-neutral
//! [`togi_simrun::day_night::successor::Record`] values, including truth-side rates and
//! certified depth objectives. The policy's own observations cross only the separate
//! [`togi_simrun::day_night::successor::PolicyReport`] interface.
//!
//! Parse the strict [`registration::Registration`] before executing an experiment and pin
//! the SHA-256 of those exact registration bytes in every record. Registered acceptance
//! requires every scenario × seed × mode × slope cell with matching provenance and the
//! complete horizon. Missing, duplicate, truncated, incompatible and invalid-execution
//! artifacts are incomplete evidence, never a gate pass. Complete unconverged trajectories
//! still contribute their observed safety; their missing convergence remains infinite.
//! Registrations name their own scenario roles and pin simulator, runner and scorer revisions.
//! A typed pending policy source can be frozen but cannot execute or score. Actual input
//! digests use [`registration::input_sha256`], not a copied expected pin. Comparator policies
//! may explicitly omit allowance/classification reports; this is not zero consumption or
//! a satisfied learning contract. Candidate reports must provide both.
//!
//! Safety and usefulness are separate. An admissible stock profile cannot manufacture
//! benign depth or convergence. Operational depth is measured against the certified value
//! over A∩E (depth at most 45), not against coordinates of a tie-broken optimum; physical
//! maxima and core means remain separate. Restoration learning after the original finish
//! boundary remains post-finish safety exposure, with no transition grace interval.
//!
//! [`score::score`] validates a registration before selecting its exact product; standalone
//! records can be decoded with [`score::read_records`]. [`render::render`] prints the summary
//! and [`render::render_seeds`] prints the unit/source accounting for retained trajectories.
//! Whole-invocation elapsed timings are deduplicated by identity, distinct from summed
//! run times; unavailable timings print as not recorded.
//!
//! Reduced development output is always labelled
//! **EXPLORATORY REDUCED DEV — NOT A REGISTERED SCORE**. The Poisson upper rate is a
//! model-based diagnostic, not an adaptive-policy guarantee or hardware reliability proof.
//! The calendar still idealizes zero setup/teardown and omits hardware placement/repeat
//! costs: these records cannot establish real-night feasibility or authorize deployment.
//! CLI execution and the separately frozen registration document are not owned here.

pub mod registration;
pub mod render;
pub mod score;

pub use registration::Registration;
