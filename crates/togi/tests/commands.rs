//! Ordinary command integration tests share one compile-and-link unit.
//! `run` stays separate because its panic and terminal fixtures re-execute that binary;
//! `vm_recovery` stays separate because the VM checks install and invoke it directly.

mod cli;
mod day;
mod doctor;
mod emergency_stop;
mod night;
mod read;
mod reset;
mod restart_limit;
mod support;
