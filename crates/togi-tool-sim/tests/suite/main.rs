#![expect(
    clippy::unwrap_used,
    reason = "fixtures panic on a broken suite or manifest"
)]

mod coverage;
mod real;
mod rules;
mod shard;
