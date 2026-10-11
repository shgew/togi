//! A gzip reader with the observable read behavior of Go's `compress/gzip.Reader` (multistream on).
//!
//! `flate2`'s gzip decoders differ from Go's in ways a trial's samples can observe: Go validates the first member's
//! header when the file is opened, ignores reserved header flag bits, caps FNAME/FCOMMENT at 512 bytes, hands
//! out every byte the inflater produced before a corrupt block, and keeps returning the same error afterwards. This
//! reader reproduces those points on top of a port of Go's own DEFLATE decoder ([`flate`]).

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, ErrorKind, Read};

use flate2::Crc;

use self::flate::{Decompressor, FlateError};

mod flate;
#[cfg(test)]
mod tests;

const ID1: u8 = 0x1f;
const ID2: u8 = 0x8b;
const CM_DEFLATE: u8 = 8;
const FLAG_HCRC: u8 = 1 << 1;
const FLAG_EXTRA: u8 = 1 << 2;
const FLAG_NAME: u8 = 1 << 3;
const FLAG_COMMENT: u8 = 1 << 4;
/// Go's `readString` buffer: the NUL terminator counts, so a name holds at most 511 bytes.
const MAX_STRING: usize = 512;
const TRAILER_LEN: usize = 8;

/// Where the reader ended up once it can produce no more bytes.
enum Terminal {
    /// A clean end of file at a member boundary.
    Eof,
    /// The first failure, replayed on every later read as Go's sticky `z.err`.
    Failed { kind: ErrorKind, message: String },
}

/// A header read failure. `Eof` is Go's `io.EOF`: zero bytes were available.
enum HeaderError {
    Eof,
    Io(io::Error),
}

impl HeaderError {
    /// Go's `noEOF`.
    fn no_eof(self) -> Self {
        match self {
            Self::Eof => Self::Io(unexpected_eof()),
            io @ Self::Io(_) => io,
        }
    }

    fn into_io(self) -> io::Error {
        match self {
            Self::Eof => io::Error::new(ErrorKind::UnexpectedEof, "EOF"),
            Self::Io(err) => err,
        }
    }
}

fn unexpected_eof() -> io::Error {
    io::Error::new(ErrorKind::UnexpectedEof, "unexpected EOF")
}

fn invalid_header() -> HeaderError {
    HeaderError::Io(io::Error::new(
        ErrorKind::InvalidData,
        "gzip: invalid header",
    ))
}

/// Reads a possibly multi-member gzip file, member headers and trailers included.
pub(crate) struct Reader {
    input: BufReader<File>,
    flate: Decompressor,
    /// CRC-32 and length of the current member's returned bytes.
    crc: Crc,
    terminal: Option<Terminal>,
}

impl fmt::Debug for Reader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reader").finish_non_exhaustive()
    }
}

impl Reader {
    /// Reads and validates the first member's header, as Go's `gzip.NewReader` does. An empty file fails with Go's
    /// `io.EOF`.
    pub(crate) fn new(file: File) -> io::Result<Self> {
        let mut reader = Self {
            input: BufReader::new(file),
            flate: Decompressor::new(),
            crc: Crc::new(),
            terminal: None,
        };
        reader.read_header().map_err(HeaderError::into_io)?;
        Ok(reader)
    }

    /// Go's `io.ReadFull`: zero bytes available is `Eof`, a short read is an unexpected EOF.
    fn read_full(&mut self, buf: &mut [u8]) -> Result<(), HeaderError> {
        let mut filled = 0;
        while let Some(rest) = buf.get_mut(filled..).filter(|rest| !rest.is_empty()) {
            match self.input.read(rest) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) => return Err(HeaderError::Io(err)),
            }
        }
        if filled == buf.len() {
            Ok(())
        } else if filled == 0 {
            Err(HeaderError::Eof)
        } else {
            Err(HeaderError::Io(unexpected_eof()))
        }
    }

    /// Go's `readString`: the NUL-terminated bytes (NUL included) are hashed into `digest`.
    fn skip_string(&mut self, digest: &mut Crc) -> Result<(), HeaderError> {
        let mut buf = [0_u8; MAX_STRING];
        for i in 0..buf.len() {
            let mut byte = [0_u8; 1];
            self.read_full(&mut byte).map_err(HeaderError::no_eof)?;
            let [b] = byte;
            if let Some(slot) = buf.get_mut(i) {
                *slot = b;
            }
            if b == 0 {
                digest.update(buf.get(..=i).unwrap_or_default());
                return Ok(());
            }
        }
        Err(invalid_header())
    }

    /// Go's `readHeader`, then arms the inflater for the member's body.
    fn read_header(&mut self) -> Result<(), HeaderError> {
        let mut fixed = [0_u8; 10];
        self.read_full(&mut fixed)?;
        let [id1, id2, cm, flags, ..] = fixed;
        if id1 != ID1 || id2 != ID2 || cm != CM_DEFLATE {
            return Err(invalid_header());
        }
        let mut digest = Crc::new();
        digest.update(&fixed);

        if flags & FLAG_EXTRA != 0 {
            let mut len = [0_u8; 2];
            self.read_full(&mut len).map_err(HeaderError::no_eof)?;
            digest.update(&len);
            let mut remaining = usize::from(u16::from_le_bytes(len));
            let mut chunk = [0_u8; MAX_STRING];
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                let part = chunk.get_mut(..n).unwrap_or_default();
                self.read_full(part).map_err(HeaderError::no_eof)?;
                digest.update(part);
                remaining -= n;
            }
        }
        if flags & FLAG_NAME != 0 {
            self.skip_string(&mut digest)?;
        }
        if flags & FLAG_COMMENT != 0 {
            self.skip_string(&mut digest)?;
        }
        if flags & FLAG_HCRC != 0 {
            let mut stored = [0_u8; 2];
            self.read_full(&mut stored).map_err(HeaderError::no_eof)?;
            let [lo, hi, ..] = digest.sum().to_le_bytes();
            if stored != [lo, hi] {
                return Err(invalid_header());
            }
        }

        self.flate.reset();
        self.crc = Crc::new();
        Ok(())
    }

    /// Ends the stream at its first failure; later reads repeat it, as Go keeps `z.err`.
    fn fail(&mut self, err: &io::Error) {
        self.terminal = Some(Terminal::Failed {
            kind: err.kind(),
            message: err.to_string(),
        });
    }

    /// The trailer check after a member's last block, then the next member's header; a clean end of file in place
    /// of that header ends the stream.
    fn finish_member(&mut self) -> io::Result<()> {
        let mut trailer = [0_u8; TRAILER_LEN];
        self.read_full(&mut trailer)
            .map_err(|err| err.no_eof().into_io())?;
        let [c0, c1, c2, c3, s0, s1, s2, s3] = trailer;
        let stored = (
            u32::from_le_bytes([c0, c1, c2, c3]),
            u32::from_le_bytes([s0, s1, s2, s3]),
        );
        if stored != (self.crc.sum(), self.crc.amount()) {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "gzip: invalid checksum",
            ));
        }
        match self.read_header() {
            Ok(()) => Ok(()),
            Err(HeaderError::Eof) => {
                self.terminal = Some(Terminal::Eof);
                Ok(())
            }
            Err(err) => Err(err.into_io()),
        }
    }
}

impl Read for Reader {
    /// Go's `Reader.Read`, which returns `(n, err)` together: here the bytes come first and `err` on the next call.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match &self.terminal {
                Some(Terminal::Eof) => return Ok(0),
                Some(Terminal::Failed { kind, message }) => {
                    return Err(io::Error::new(*kind, message.clone()));
                }
                None => {}
            }
            if buf.is_empty() {
                return Ok(0);
            }
            let (n, flate_err) = self.flate.read(&mut self.input, buf);
            self.crc.update(buf.get(..n).unwrap_or_default());
            let step = match flate_err {
                None => Ok(()),
                Some(FlateError::End) => self.finish_member(),
                Some(err) => Err(err.into_io()),
            };
            if let Err(err) = step {
                self.fail(&err);
            }
            if n > 0 {
                return Ok(n);
            }
        }
    }
}
