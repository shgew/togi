//! Hardware access through `ryzen_smu`: [`Driver`] validates the CPU and driver, reads topology and
//! BIOS context, and reads and writes offsets; [`PmReader`] supplies PM table telemetry.

mod driver;
mod error;
mod files;
mod identity;
mod mailbox;
mod pm_table;
mod slots;
mod sysfs;
mod topology;

pub use driver::Driver;
pub use error::{Error, Operation};
pub use identity::CpuSummary;
pub use mailbox::{Command, MailboxError, Request, Status};
pub use pm_table::{DecodeError, Lane, PmReader, Unavailable};
pub use slots::{FuseRead, Misplaced, SlotError};

#[cfg(test)]
mod tests;
