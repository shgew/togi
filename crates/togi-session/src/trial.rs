//! Starting a trial.

use togi_journal::Seq;

use crate::write::Verified;

/// The right to start a trial on the profile a [`Verified`] write transaction confirmed. The trial's records cite
/// the readbacks that confirmed it.
///
/// There is no way to start a trial without the proof:
///
/// ```compile_fail,E0061
/// let _ = togi_session::trial::authorize();
/// ```
///
/// ```compile_fail,E0308
/// let _ = togi_session::trial::authorize(&());
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Authorized<'v> {
    readbacks: &'v [Seq],
}

impl Authorized<'_> {
    pub const fn readbacks(&self) -> &[Seq] {
        self.readbacks
    }
}

pub fn authorize(verified: &Verified) -> Authorized<'_> {
    Authorized {
        readbacks: verified.readbacks(),
    }
}
