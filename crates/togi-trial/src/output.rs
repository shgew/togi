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
    /// After an oversized line nothing more is consumed.
    pub(crate) fn push(
        &mut self,
        bytes: &[u8],
        sink: &mut impl FnMut(Frame) -> ControlFlow<()>,
    ) -> usize {
        for (index, &byte) in bytes.iter().enumerate() {
            if self.exceeded {
                return index;
            }
            if byte == b'\n' || byte == b'\r' {
                let line = String::from_utf8_lossy(&self.line).into_owned();
                self.line.clear();
                if sink(Frame::Line(line)).is_break() {
                    return index + 1;
                }
            } else if self.line.len() == LINE_LIMIT {
                self.exceeded = true;
                let prefix = std::mem::take(&mut self.line);
                let _ = sink(Frame::Exceeded { prefix });
                return index;
            } else {
                self.line.push(byte);
            }
        }
        bytes.len()
    }

    /// The unterminated line left at end of input; none when the line was oversized.
    pub(crate) fn finish(&mut self) -> Option<String> {
        if self.exceeded || self.line.is_empty() {
            return None;
        }
        let line = String::from_utf8_lossy(&self.line).into_owned();
        self.line.clear();
        Some(line)
    }

    /// Whether an unterminated line is buffered.
    pub(crate) const fn pending(&self) -> bool {
        !self.line.is_empty()
    }

    pub(crate) const fn exceeded(&self) -> bool {
        self.exceeded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(chunks: &[&[u8]]) -> (Vec<Frame>, Framer) {
        let mut framer = Framer::default();
        let mut out = Vec::new();
        for chunk in chunks {
            framer.push(chunk, &mut |frame| {
                out.push(frame);
                ControlFlow::Continue(())
            });
        }
        (out, framer)
    }

    fn line(text: &str) -> Frame {
        Frame::Line(text.to_owned())
    }

    #[test]
    fn lf_cr_and_crlf_each_end_a_line() {
        let (out, mut framer) = frames(&[b"a\nb\rc\r\nd"]);
        assert_eq!(out, [line("a"), line("b"), line("c"), line(""),]);
        assert_eq!(framer.finish().as_deref(), Some("d"));
    }

    #[test]
    fn lines_split_across_chunks_join() {
        let (out, mut framer) = frames(&[b"CO", b"MPUTE ERR", b"OR\nnext", b"\n"]);
        assert_eq!(out, [line("COMPUTE ERROR"), line("next")]);
        assert_eq!(framer.finish(), None);
    }

    #[test]
    fn a_line_of_exactly_the_limit_is_accepted_terminated_or_not() {
        let exact = vec![b'x'; LINE_LIMIT];
        let mut terminated = exact.clone();
        terminated.push(b'\n');
        let (out, framer) = frames(&[&terminated]);
        assert_eq!(out.len(), 1);
        assert!(!framer.exceeded());
        let (out, mut framer) = frames(&[&exact]);
        assert!(out.is_empty());
        assert!(!framer.exceeded());
        assert_eq!(framer.finish().map(|l| l.len()), Some(LINE_LIMIT));
    }

    #[test]
    fn one_byte_over_the_limit_exceeds_and_keeps_the_prefix() {
        let over = vec![b'x'; LINE_LIMIT + 1];
        let (out, mut framer) = frames(&[&over]);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Frame::Exceeded { prefix } if prefix.len() == LINE_LIMIT));
        assert!(framer.exceeded());
        assert_eq!(framer.finish(), None);
    }

    #[test]
    fn nothing_is_consumed_after_exceeding() {
        let over = vec![b'x'; LINE_LIMIT + 10];
        let mut framer = Framer::default();
        let consumed = framer.push(&over, &mut |_| ControlFlow::Continue(()));
        assert_eq!(consumed, LINE_LIMIT);
        assert_eq!(
            framer.push(b"more\n", &mut |_| ControlFlow::Continue(())),
            0
        );
    }

    #[test]
    fn breaking_after_a_line_reports_the_exact_bytes_consumed() {
        let mut framer = Framer::default();
        let mut seen = 0;
        let consumed = framer.push(b"one\ntwo\nthree\n", &mut |_| {
            seen += 1;
            if seen == 2 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        assert_eq!(consumed, 8);
        assert_eq!(
            framer.push(&b"one\ntwo\nthree\n"[consumed..], &mut |_| {
                ControlFlow::Continue(())
            }),
            6
        );
    }

    #[test]
    fn invalid_utf8_is_replaced_not_dropped() {
        let (out, _) = frames(&[b"a\xffb\n"]);
        assert_eq!(out, [line("a\u{fffd}b")]);
    }

    #[test]
    fn memory_stays_bounded_by_the_limit() {
        let mut framer = Framer::default();
        let chunk = vec![b'y'; CHUNK];
        for _ in 0..(4 * LINE_LIMIT / CHUNK) {
            framer.push(&chunk, &mut |_| ControlFlow::Continue(()));
        }
        assert!(framer.line.capacity() <= 2 * LINE_LIMIT);
    }
}
