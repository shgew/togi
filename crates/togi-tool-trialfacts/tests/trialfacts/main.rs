#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "helpers panic on invalid fixtures and keep simulated journals in memory"
)]

mod builders;
mod committed;
mod extract;
mod file;
mod key;
mod record;
mod replay;
