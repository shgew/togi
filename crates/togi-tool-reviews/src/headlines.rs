//! Changed lines numbered in the frozen head, distinct from commit-local patch spans.

use serde::{Deserialize, Serialize};

use crate::diff::Span;

/// An inclusive range in the frozen head's version of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadSpan {
    first: i64,
    last: i64,
}

impl HeadSpan {
    /// Takes the new side of a diff whose destination is the frozen head.
    pub fn from_new_side(span: Span) -> Self {
        Self {
            first: span.first,
            last: span.last,
        }
    }
    pub fn first(self) -> i64 {
        self.first
    }

    pub fn last(self) -> i64 {
        self.last
    }

}
