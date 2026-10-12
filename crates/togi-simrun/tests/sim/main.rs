#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

#[macro_use]
mod support;

mod cap;
mod carry;
mod common;
mod config;
mod crash;
mod first_result;
mod forecast;
mod hunt;
mod memo;
mod memory;
mod persistence;
mod r7_chain;
mod samples;
mod sixteen;
mod stop;
mod warm_cold;
