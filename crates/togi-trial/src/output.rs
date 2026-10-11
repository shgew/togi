use std::ops::ControlFlow;

/// A backend output line may not exceed this many bytes before its terminator.
pub(crate) const LINE_LIMIT: usize = 64 * 1024;
/// Pipes and watched files are read in chunks of this many bytes.
pub(crate) const CHUNK: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Frame {
    /// One complete line without its terminator (CR and LF each end a line).
    Line(String),
    /// The line passed `LINE_LIMIT`; the first `LINE_LIMIT` bytes are kept as a diagnostic prefix
    /// and the stream is not read further.
    Exceeded { prefix: Vec<u8> },
}

/// Splits a byte stream into lines with a fixed memory bound.
#[derive(Debug, Default)]
pub(crate) struct Framer {
    line: Vec<u8>,
    exceeded: bool,
}

impl Framer {
    /// Consumes bytes up to and including the delimiter after which `sink` breaks, or the whole
    /// slice, and returns how many bytes it consumed so a caller can resume at an exact offset.
    pub(crate) fn push(
        &mut self,
        bytes: &[u8],
        sink: &mut impl FnMut(Frame) -> ControlFlow<()>,
    ) -> usize {
        let _ = (bytes, sink);
        unimplemented!()
    }

    /// The unterminated line left at end of input; none when the line was oversized.
    pub(crate) fn finish(&mut self) -> Option<String> {
        unimplemented!()
    }

    pub(crate) fn exceeded(&self) -> bool {
        self.exceeded
    }
}
