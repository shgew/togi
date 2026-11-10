//! Hardware access through `ryzen_smu`: [`Driver`] validates the CPU and driver, reads topology and
//! BIOS context, and reads and writes offsets; [`PmReader`] supplies PM table telemetry.
//! [`OffsetWitness`] supplies a checked nonuniform fixture and readback validation for
//! the journaled H1 witness. It performs no writes; production writes remain owned by
//! the session transaction. The opt-in maintainer hardware test restores verified stock.

mod driver;
mod error;
mod files;
mod identity;
mod mailbox;
mod pm_table;
mod slots;
mod sysfs;
mod topology;
mod witness;

pub use driver::Driver;
pub use error::{Error, Operation};
pub use identity::CpuSummary;
pub use mailbox::{Command, MailboxError, Request, Status};
pub use pm_table::{DecodeError, Lane, PmReader, Unavailable};
pub use slots::{FuseRead, Misplaced, SlotError};
pub use witness::{OffsetWitness, WitnessError};

#[cfg(test)]
mod tests;
