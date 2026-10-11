use jiff::Timestamp;

use super::{DirError, StateDir};
use crate::session_id::SessionId;

impl StateDir {
    /// The first session id for `now` that no archive, trial directory or marker has and that sorts after the reset
    /// boundary, so a clock set back cannot reuse a discarded lineage.
    pub fn session_id(&self, now: Timestamp) -> Result<SessionId, DirError> {
        let mut candidate = SessionId::new(now, None).map_err(|source| DirError::BadSession {
            path: self.path().to_path_buf(),
            source,
        })?;
        if let Some(boundary) = self.reset_boundary()?
            && candidate <= boundary
        {
            candidate = boundary.next_suffix().ok_or(DirError::SessionIdsExhausted { after: boundary })?;
        }
        loop {
            match self.check_free(&candidate) {
                Ok(()) => return Ok(candidate),
                Err(DirError::ArchiveExists { .. }) => {
                    candidate = candidate.next_suffix().ok_or(DirError::SessionIdsExhausted { after: candidate })?;
                }
                Err(error) => return Err(error),
            }
        }
    }
}
