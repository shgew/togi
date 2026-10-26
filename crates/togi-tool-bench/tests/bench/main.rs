#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

mod common;
mod fake;
mod git;
mod session;

mod archive;
mod cache;
mod cli;
mod compare;
mod measure;
mod records;
mod research;
mod run;
mod same;
mod sanity;
mod suite;
