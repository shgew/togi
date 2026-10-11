#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "helpers panic on a broken fixture, and the tests keep journals in memory and dump them to disk"
)]

mod builders;
mod committed;
mod extract;
mod file;
mod key;
mod record;
mod replay;
