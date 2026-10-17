#![expect(
    clippy::unwrap_used,
    clippy::needless_pass_by_value,
    reason = "fixtures panic on a broken journal and take JSON literals by value"
)]

mod avoidance;
mod conclusion;
mod directory;
mod fixtures;
mod ranges;
mod rebuild;
mod references;
mod report;
mod snapshot;
mod writes;
