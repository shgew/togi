#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    clippy::needless_pass_by_value,
    reason = "fixtures panic on a broken journal, take JSON literals by value, and keep state directories in memory"
)]

mod avoidance;
mod conclusion;
mod directory;
mod fixtures;
mod ranges;
mod rebuild;
mod references;
mod snapshot;
mod sweep;
mod writes;
