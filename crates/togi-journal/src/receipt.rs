use crate::event::Seq;
use crate::payload::SmuCommand;

/// Proof that the journal made a command's `smu.intent` durable, issued by [`Journal::append_smu_intent`] and spent
/// by [`Journal::append_smu_write`] on the same journal.
///
/// A receipt cannot be built, cloned or copied outside this crate:
///
/// ```compile_fail,E0451
/// fn forge(intent: togi_journal::Seq, command: togi_journal::SmuCommand) -> togi_journal::SmuIntentReceipt {
///     togi_journal::SmuIntentReceipt { intent, command }
/// }
/// ```
///
/// ```compile_fail,E0599
/// fn clone(receipt: &togi_journal::SmuIntentReceipt) -> togi_journal::SmuIntentReceipt {
///     receipt.clone()
/// }
/// ```
///
/// ```compile_fail,E0382
/// fn twice(receipt: togi_journal::SmuIntentReceipt) -> (togi_journal::SmuIntentReceipt, togi_journal::SmuIntentReceipt) {
///     (receipt, receipt)
/// }
/// ```
///
/// [`Journal::append_smu_intent`]: crate::Journal::append_smu_intent
/// [`Journal::append_smu_write`]: crate::Journal::append_smu_write
#[must_use = "the receipt authorizes one SMU write"]
#[derive(Debug)]
pub struct SmuIntentReceipt {
    journal: JournalId,
    intent: Seq,
    command: SmuCommand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JournalId(u64);

impl JournalId {
    pub(crate) fn next() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

impl SmuIntentReceipt {
    pub(crate) const fn new(journal: JournalId, intent: Seq, command: SmuCommand) -> Self {
        Self {
            journal,
            intent,
            command,
        }
    }

    pub(crate) fn into_parts(self) -> (JournalId, Seq, SmuCommand) {
        (self.journal, self.intent, self.command)
    }

    /// The seq of the durable `smu.intent`.
    pub const fn intent(&self) -> Seq {
        self.intent
    }

    pub const fn command(&self) -> SmuCommand {
        self.command
    }
}

/// The durable `smu.write` of a command and the intent that authorized it; the readback cites `seq`.
#[must_use = "the readback must cite the write"]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmuWritten {
    seq: Seq,
    intent: Seq,
    command: SmuCommand,
}

impl SmuWritten {
    pub(crate) const fn new(seq: Seq, intent: Seq, command: SmuCommand) -> Self {
        Self {
            seq,
            intent,
            command,
        }
    }

    pub const fn seq(&self) -> Seq {
        self.seq
    }

    pub const fn intent(&self) -> Seq {
        self.intent
    }

    pub const fn command(&self) -> SmuCommand {
        self.command
    }
}
