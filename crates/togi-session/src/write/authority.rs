use togi_journal::{Seq, SmuCommand, SmuIntentReceipt};
use togi_machine::{
    CancellationToken, Error, FrequencyJournal, Partial, Running, Smu, Stopped, TrialSpec, Trials,
};

/// Proof that every command of a transaction reached the machine and its readbacks agreed. Only the write
/// transaction builds one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Verified {
    readbacks: Vec<Seq>,
}

impl Verified {
    pub(super) const fn new(readbacks: Vec<Seq>) -> Self {
        Self { readbacks }
    }

    /// The `smu.readback` events that confirmed it, oldest first.
    pub(crate) fn readbacks(&self) -> &[Seq] {
        &self.readbacks
    }
}

/// The right to start a trial on the profile a [`Verified`] write transaction confirmed. The trial's records cite
/// the readbacks that confirmed it. Only [`authorize`] makes one, and only from a [`Verified`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Authorized<'v> {
    readbacks: &'v [Seq],
}

impl Authorized<'_> {
    pub(crate) const fn readbacks(&self) -> &[Seq] {
        self.readbacks
    }
}

pub(crate) fn authorize(verified: &Verified) -> Authorized<'_> {
    Authorized {
        readbacks: verified.readbacks(),
    }
}

/// Sends the command the receipt's durable intent names to the SMU: the only caller of the seam's writes.
#[expect(
    clippy::disallowed_methods,
    reason = "the write transaction is the only caller of the SMU write seam"
)]
pub(crate) fn execute(smu: &mut dyn Smu, receipt: &SmuIntentReceipt) -> Result<(), Error> {
    match receipt.command() {
        SmuCommand::Set { core, offset } => smu.set_offset(core, offset),
        SmuCommand::SetAll { offset } => smu.set_all_offsets(offset),
    }
}

/// Starts a trial: the only caller of the seam's `start`, which takes the proof that nothing else runs and gives it
/// back only with a failed start whose rollback confirmed it.
pub(crate) fn start(
    trials: &mut dyn Trials,
    cancel: &CancellationToken,
    _authorized: Authorized<'_>,
    spec: TrialSpec,
    ready: Stopped,
    journal: &mut dyn FrequencyJournal,
) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
    trials.start(cancel, spec, ready, journal)
}
