//! A port of Go 1.27.1's `compress/flate` decompressor (`inflate.go`, `dict_decoder.go`).
//!
//! The port keeps Go's observable points on damaged input: which bytes come out before an error, the input offset in
//! `flate: corrupt input before offset N`, the literal decoder's lookahead raised to the end-of-block code length, and
//! back references limited to the current member's history.

use std::io::{self, BufRead, ErrorKind};

const MAX_CODE_LEN: usize = 16;
const MAX_NUM_LIT: usize = 286;
const MAX_NUM_DIST: usize = 30;
const NUM_CODES: usize = 19;
const END_BLOCK_MARKER: usize = 256;
/// Go's `maxMatchOffset`, the history window.
const WINDOW: usize = 1 << 15;

const CHUNK_BITS: u32 = 9;
const NUM_CHUNKS: usize = 1 << CHUNK_BITS;
const COUNT_MASK: u32 = 15;
const VALUE_SHIFT: u32 = 4;

const CODE_ORDER: [usize; NUM_CODES] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// A decompressor failure, kept and returned again like Go's `f.err`.
#[derive(Clone, Debug)]
pub(super) enum FlateError {
    /// The final block ended: Go's `io.EOF`.
    End,
    /// Go's `io.ErrUnexpectedEOF`.
    UnexpectedEof,
    /// Go's `CorruptInputError`.
    Corrupt(i64),
    /// A failed read of the underlying file.
    Io(ErrorKind, String),
}

impl FlateError {
    pub(super) fn into_io(self) -> io::Error {
        match self {
            Self::End => io::Error::new(ErrorKind::UnexpectedEof, "EOF"),
            Self::UnexpectedEof => io::Error::new(ErrorKind::UnexpectedEof, "unexpected EOF"),
            Self::Corrupt(offset) => io::Error::new(
                ErrorKind::InvalidData,
                format!("flate: corrupt input before offset {offset}"),
            ),
            Self::Io(kind, message) => io::Error::new(kind, message),
        }
    }
}

/// Reads at most one byte: `Ok(None)` is end of file, as Go's `ReadByte` returning `io.EOF`.
fn read_byte<R: BufRead>(src: &mut R) -> Result<Option<u8>, FlateError> {
    loop {
        match src.fill_buf() {
            Ok(buf) => {
                let Some(&byte) = buf.first() else {
                    return Ok(None);
                };
                src.consume(1);
                return Ok(Some(byte));
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(FlateError::Io(err.kind(), err.to_string())),
        }
    }
}

/// Go's `io.ReadFull` followed by `noEOF`: the bytes read, and the error if `buf` was not filled.
fn read_full<R: BufRead>(src: &mut R, buf: &mut [u8]) -> (usize, Option<FlateError>) {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..).filter(|rest| !rest.is_empty()) {
        match src.read(rest) {
            Ok(0) => return (filled, Some(FlateError::UnexpectedEof)),
            Ok(n) => filled += n,
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return (filled, Some(FlateError::Io(err.kind(), err.to_string()))),
        }
    }
    (filled, None)
}

fn reverse_bits(code: usize, len: u32) -> usize {
    usize::from(
        u16::try_from(code & 0xffff)
            .unwrap_or_default()
            .reverse_bits(),
    ) >> (16 - len)
}

/// Go's `huffmanDecoder`: 9-bit chunks with overflow links for longer codes.
struct Huffman {
    min: u32,
    chunks: Box<[u32; NUM_CHUNKS]>,
    links: Vec<Vec<u32>>,
    link_mask: u32,
}

impl Huffman {
    fn new() -> Self {
        Self {
            min: 0,
            chunks: Box::new([0; NUM_CHUNKS]),
            links: Vec::new(),
            link_mask: 0,
        }
    }

    /// Go's `huffmanDecoder.init`: false when the lengths do not form a complete code (one 1-bit code is allowed).
    fn init(&mut self, lengths: &[u32]) -> bool {
        if self.min != 0 {
            self.min = 0;
            self.chunks.fill(0);
            self.links.clear();
            self.link_mask = 0;
        }

        let mut count = [0_usize; MAX_CODE_LEN];
        let (mut min, mut max) = (0_u32, 0_u32);
        for &n in lengths {
            if n == 0 {
                continue;
            }
            if min == 0 || n < min {
                min = n;
            }
            max = max.max(n);
            if let Some(c) = count.get_mut(n as usize) {
                *c += 1;
            }
        }
        if max == 0 {
            return true;
        }

        let mut code = 0_usize;
        let mut next_code = [0_usize; MAX_CODE_LEN];
        for i in min..=max {
            code <<= 1;
            if let Some(next) = next_code.get_mut(i as usize) {
                *next = code;
            }
            code += count.get(i as usize).copied().unwrap_or_default();
        }
        if code != 1 << max && !(code == 1 && max == 1) {
            return false;
        }

        self.min = min;
        if max > CHUNK_BITS {
            let num_links = 1_usize << (max - CHUNK_BITS);
            self.link_mask = u32::try_from(num_links - 1).unwrap_or(u32::MAX);
            let link = next_code[CHUNK_BITS as usize + 1] >> 1;
            self.links = vec![Vec::new(); NUM_CHUNKS.saturating_sub(link)];
            for j in link..NUM_CHUNKS {
                let off = j - link;
                if let Some(chunk) = self.chunks.get_mut(reverse_bits(j, CHUNK_BITS)) {
                    *chunk =
                        u32::try_from(off << VALUE_SHIFT).unwrap_or_default() | (CHUNK_BITS + 1);
                }
                if let Some(table) = self.links.get_mut(off) {
                    *table = vec![0; num_links];
                }
            }
        }

        for (symbol, &n) in lengths.iter().enumerate() {
            if n == 0 {
                continue;
            }
            let Some(next) = next_code.get_mut(n as usize) else {
                continue;
            };
            let code = *next;
            *next += 1;
            let chunk = u32::try_from(symbol << VALUE_SHIFT).unwrap_or_default() | n;
            let reverse = reverse_bits(code, n);
            if n <= CHUNK_BITS {
                for slot in self.chunks.iter_mut().skip(reverse).step_by(1 << n) {
                    *slot = chunk;
                }
            } else {
                let value = self
                    .chunks
                    .get(reverse & (NUM_CHUNKS - 1))
                    .copied()
                    .unwrap_or_default()
                    >> VALUE_SHIFT;
                if let Some(table) = self.links.get_mut(value as usize) {
                    for slot in table
                        .iter_mut()
                        .skip(reverse >> CHUNK_BITS)
                        .step_by(1 << (n - CHUNK_BITS))
                    {
                        *slot = chunk;
                    }
                }
            }
        }
        true
    }

    fn fixed() -> Self {
        let mut lengths = [0_u32; 288];
        for (symbol, len) in lengths.iter_mut().enumerate() {
            *len = match symbol {
                0..144 => 8,
                144..256 => 9,
                256..280 => 7,
                _ => 8,
            };
        }
        let mut huffman = Self::new();
        huffman.init(&lengths);
        huffman
    }
}

/// Go's `dictDecoder`: the history window, which doubles as the output buffer.
struct Dict {
    hist: Vec<u8>,
    wr_pos: usize,
    rd_pos: usize,
    full: bool,
}

impl Dict {
    fn reset(&mut self) {
        self.wr_pos = 0;
        self.rd_pos = 0;
        self.full = false;
    }

    fn hist_size(&self) -> usize {
        if self.full {
            self.hist.len()
        } else {
            self.wr_pos
        }
    }

    fn avail_read(&self) -> usize {
        self.wr_pos - self.rd_pos
    }

    fn avail_write(&self) -> usize {
        self.hist.len() - self.wr_pos
    }

    fn write_byte(&mut self, byte: u8) {
        if let Some(slot) = self.hist.get_mut(self.wr_pos) {
            *slot = byte;
        }
        self.wr_pos += 1;
    }

    /// Go's `copy(hist[dst:end], hist[src:src_end])`: copies the shorter length and returns it.
    fn copy_within(&mut self, dst: usize, end: usize, src: usize, src_end: usize) -> usize {
        let n = (end - dst).min(src_end - src);
        self.hist.copy_within(src..src + n, dst);
        n
    }

    fn write_copy(&mut self, dist: usize, length: usize) -> usize {
        let dst_base = self.wr_pos;
        let mut dst = dst_base;
        let end = (dst + length).min(self.hist.len());
        let mut src = dst.wrapping_sub(dist);
        if dist > dst {
            src = dst + self.hist.len() - dist;
            dst += self.copy_within(dst, end, src, self.hist.len());
            src = 0;
        }
        while dst < end {
            dst += self.copy_within(dst, end, src, dst);
        }
        self.wr_pos = dst;
        dst - dst_base
    }

    fn try_write_copy(&mut self, dist: usize, length: usize) -> usize {
        let mut dst = self.wr_pos;
        let end = dst + length;
        if dst < dist || end > self.hist.len() {
            return 0;
        }
        let dst_base = dst;
        let src = dst - dist;
        while dst < end {
            dst += self.copy_within(dst, end, src, dst);
        }
        self.wr_pos = dst;
        dst - dst_base
    }

    /// Go's `readFlush`: the range of `hist` to hand out next.
    fn read_flush(&mut self) -> (usize, usize) {
        let range = (self.rd_pos, self.wr_pos);
        self.rd_pos = self.wr_pos;
        if self.wr_pos == self.hist.len() {
            self.wr_pos = 0;
            self.rd_pos = 0;
            self.full = true;
        }
        range
    }
}

#[derive(Clone, Copy)]
enum Step {
    NextBlock,
    /// `huffmanBlock`; `true` resumes at `copyHistory` (Go's `stateDict`).
    Huffman {
        copying: bool,
    },
    CopyData,
}

#[derive(Clone, Copy)]
enum Table {
    Fixed,
    Dynamic,
}

/// The bit reader state shared by every step.
struct Bits {
    b: u32,
    nb: u32,
    roffset: i64,
}

impl Bits {
    /// Go's `moreBits`.
    fn more<R: BufRead>(&mut self, src: &mut R) -> Result<(), FlateError> {
        let byte = read_byte(src)?.ok_or(FlateError::UnexpectedEof)?;
        self.roffset += 1;
        self.b |= u32::from(byte).checked_shl(self.nb).unwrap_or(0);
        self.nb += 8;
        Ok(())
    }

    fn need<R: BufRead>(&mut self, src: &mut R, n: u32) -> Result<(), FlateError> {
        while self.nb < n {
            self.more(src)?;
        }
        Ok(())
    }

    fn take(&mut self, n: u32) -> u32 {
        let value = self.b & ((1_u32 << n) - 1);
        self.b >>= n;
        self.nb -= n;
        value
    }

    /// Go's `huffSym`: reads at least `h.min` bits before the first lookup.
    fn huff_sym<R: BufRead>(&mut self, src: &mut R, h: &Huffman) -> Result<usize, FlateError> {
        let mut n = h.min;
        let (mut nb, mut b) = (self.nb, self.b);
        loop {
            while nb < n {
                let Some(byte) = read_byte(src)? else {
                    self.b = b;
                    self.nb = nb;
                    return Err(FlateError::UnexpectedEof);
                };
                self.roffset += 1;
                b |= u32::from(byte).wrapping_shl(nb);
                nb += 8;
            }
            let mut chunk = h
                .chunks
                .get((b as usize) & (NUM_CHUNKS - 1))
                .copied()
                .unwrap_or_default();
            n = chunk & COUNT_MASK;
            if n > CHUNK_BITS {
                chunk = h
                    .links
                    .get((chunk >> VALUE_SHIFT) as usize)
                    .and_then(|table| table.get(((b >> CHUNK_BITS) & h.link_mask) as usize))
                    .copied()
                    .unwrap_or_default();
                n = chunk & COUNT_MASK;
            }
            if n <= nb {
                if n == 0 {
                    self.b = b;
                    self.nb = nb;
                    return Err(FlateError::Corrupt(self.roffset));
                }
                self.b = b.wrapping_shr(n);
                self.nb = nb - n;
                return Ok((chunk >> VALUE_SHIFT) as usize);
            }
        }
    }
}

/// Go's `decompressor`, reset for each gzip member.
pub(super) struct Decompressor {
    bits: Bits,
    h1: Huffman,
    h2: Huffman,
    fixed: Huffman,
    lengths: Box<[u32; MAX_NUM_LIT + MAX_NUM_DIST]>,
    code_lengths: [u32; NUM_CODES],
    dict: Dict,
    step: Step,
    is_final: bool,
    err: Option<FlateError>,
    to_read: (usize, usize),
    literals: Table,
    dynamic_distances: bool,
    copy_len: usize,
    copy_dist: usize,
}

impl Decompressor {
    pub(super) fn new() -> Self {
        Self {
            bits: Bits {
                b: 0,
                nb: 0,
                roffset: 0,
            },
            h1: Huffman::new(),
            h2: Huffman::new(),
            fixed: Huffman::fixed(),
            lengths: Box::new([0; MAX_NUM_LIT + MAX_NUM_DIST]),
            code_lengths: [0; NUM_CODES],
            dict: Dict {
                hist: vec![0; WINDOW],
                wr_pos: 0,
                rd_pos: 0,
                full: false,
            },
            step: Step::NextBlock,
            is_final: false,
            err: None,
            to_read: (0, 0),
            literals: Table::Fixed,
            dynamic_distances: false,
            copy_len: 0,
            copy_dist: 0,
        }
    }

    /// Go's `Reset(r, nil)`: a new stream with an empty history.
    pub(super) fn reset(&mut self) {
        self.bits = Bits {
            b: 0,
            nb: 0,
            roffset: 0,
        };
        self.dict.reset();
        self.step = Step::NextBlock;
        self.is_final = false;
        self.err = None;
        self.to_read = (0, 0);
        self.copy_len = 0;
        self.copy_dist = 0;
    }

    /// Go's `Read`: the bytes copied into `buf` and the error that came with them, if any.
    pub(super) fn read<R: BufRead>(
        &mut self,
        src: &mut R,
        buf: &mut [u8],
    ) -> (usize, Option<FlateError>) {
        loop {
            let (start, end) = self.to_read;
            if start < end {
                let n = (end - start).min(buf.len());
                if let (Some(out), Some(data)) =
                    (buf.get_mut(..n), self.dict.hist.get(start..start + n))
                {
                    out.copy_from_slice(data);
                }
                self.to_read.0 += n;
                if self.to_read.0 == end {
                    return (n, self.err.clone());
                }
                return (n, None);
            }
            if let Some(err) = &self.err {
                return (0, Some(err.clone()));
            }
            match self.step {
                Step::NextBlock => self.next_block(src),
                Step::Huffman { copying } => self.huffman_block(src, copying),
                Step::CopyData => self.copy_data(src),
            }
            if self.err.is_some() && self.to_read.0 == self.to_read.1 {
                self.to_read = self.dict.read_flush();
            }
        }
    }

    fn next_block<R: BufRead>(&mut self, src: &mut R) {
        if let Err(err) = self.bits.need(src, 1 + 2) {
            self.err = Some(err);
            return;
        }
        self.is_final = self.bits.take(1) == 1;
        match self.bits.take(2) {
            0 => self.data_block(src),
            1 => {
                self.literals = Table::Fixed;
                self.dynamic_distances = false;
                self.huffman_block(src, false);
            }
            2 => {
                if let Err(err) = self.read_huffman(src) {
                    self.err = Some(err);
                    return;
                }
                self.literals = Table::Dynamic;
                self.dynamic_distances = true;
                self.huffman_block(src, false);
            }
            _ => self.err = Some(FlateError::Corrupt(self.bits.roffset)),
        }
    }

    /// Go's `readHuffman`: the dynamic block's code length tables.
    fn read_huffman<R: BufRead>(&mut self, src: &mut R) -> Result<(), FlateError> {
        self.bits.need(src, 5 + 5 + 4)?;
        let nlit = self.bits.b as usize & 0x1f;
        let nlit = nlit + 257;
        if nlit > MAX_NUM_LIT {
            return Err(FlateError::Corrupt(self.bits.roffset));
        }
        self.bits.b >>= 5;
        let ndist = (self.bits.b as usize & 0x1f) + 1;
        if ndist > MAX_NUM_DIST {
            return Err(FlateError::Corrupt(self.bits.roffset));
        }
        self.bits.b >>= 5;
        let nclen = (self.bits.b as usize & 0xf) + 4;
        self.bits.b >>= 4;
        self.bits.nb -= 5 + 5 + 4;

        for (i, &symbol) in CODE_ORDER.iter().enumerate() {
            let len = if i < nclen {
                self.bits.need(src, 3)?;
                self.bits.take(3)
            } else {
                0
            };
            if let Some(slot) = self.code_lengths.get_mut(symbol) {
                *slot = len;
            }
        }
        if !self.h1.init(&self.code_lengths) {
            return Err(FlateError::Corrupt(self.bits.roffset));
        }

        let n = nlit + ndist;
        let mut i = 0;
        while i < n {
            let x = self.bits.huff_sym(src, &self.h1)?;
            if x < 16 {
                if let Some(slot) = self.lengths.get_mut(i) {
                    *slot = u32::try_from(x).unwrap_or_default();
                }
                i += 1;
                continue;
            }
            let (mut rep, nb, b) = match x {
                16 => {
                    let Some(&previous) = i.checked_sub(1).and_then(|p| self.lengths.get(p)) else {
                        return Err(FlateError::Corrupt(self.bits.roffset));
                    };
                    (3, 2, previous)
                }
                17 => (3, 3, 0),
                _ => (11, 7, 0),
            };
            self.bits.need(src, nb)?;
            rep += self.bits.take(nb) as usize;
            if i + rep > n {
                return Err(FlateError::Corrupt(self.bits.roffset));
            }
            for slot in self.lengths.iter_mut().skip(i).take(rep) {
                *slot = b;
            }
            i += rep;
        }

        let (lit, dist) = self.lengths.split_at(nlit);
        if !self.h1.init(lit) || !self.h2.init(dist.get(..ndist).unwrap_or_default()) {
            return Err(FlateError::Corrupt(self.bits.roffset));
        }
        // Go raises the literal decoder's lookahead to the end-of-block code length.
        let eob = self
            .lengths
            .get(END_BLOCK_MARKER)
            .copied()
            .unwrap_or_default();
        self.h1.min = self.h1.min.max(eob);
        Ok(())
    }

    /// Go's `huffmanBlock`; `copying` resumes a back reference that filled the window.
    fn huffman_block<R: BufRead>(&mut self, src: &mut R, mut copying: bool) {
        loop {
            if !copying {
                let literals = match self.literals {
                    Table::Fixed => &self.fixed,
                    Table::Dynamic => &self.h1,
                };
                let v = match self.bits.huff_sym(src, literals) {
                    Ok(v) => v,
                    Err(err) => {
                        self.err = Some(err);
                        return;
                    }
                };
                let (mut length, n) = match v {
                    0..256 => {
                        self.dict.write_byte(u8::try_from(v).unwrap_or_default());
                        if self.dict.avail_write() == 0 {
                            self.to_read = self.dict.read_flush();
                            self.step = Step::Huffman { copying: false };
                            return;
                        }
                        continue;
                    }
                    256 => {
                        self.finish_block();
                        return;
                    }
                    257..265 => (v - (257 - 3), 0),
                    265..269 => (v * 2 - (265 * 2 - 11), 1),
                    269..273 => (v * 4 - (269 * 4 - 19), 2),
                    273..277 => (v * 8 - (273 * 8 - 35), 3),
                    277..281 => (v * 16 - (277 * 16 - 67), 4),
                    281..285 => (v * 32 - (281 * 32 - 131), 5),
                    285 => (258, 0),
                    _ => {
                        self.err = Some(FlateError::Corrupt(self.bits.roffset));
                        return;
                    }
                };
                if n > 0 {
                    if let Err(err) = self.bits.need(src, n) {
                        self.err = Some(err);
                        return;
                    }
                    length += self.bits.take(n) as usize;
                }

                let dist = if self.dynamic_distances {
                    match self.bits.huff_sym(src, &self.h2) {
                        Ok(dist) => dist,
                        Err(err) => {
                            self.err = Some(err);
                            return;
                        }
                    }
                } else {
                    if let Err(err) = self.bits.need(src, 5) {
                        self.err = Some(err);
                        return;
                    }
                    let code = u8::try_from(self.bits.take(5) << 3).unwrap_or_default();
                    usize::from(code.reverse_bits())
                };
                let dist = match dist {
                    0..4 => dist + 1,
                    4..MAX_NUM_DIST => {
                        let nb = u32::try_from((dist - 2) >> 1).unwrap_or_default();
                        let mut extra = (dist & 1) << nb;
                        if let Err(err) = self.bits.need(src, nb) {
                            self.err = Some(err);
                            return;
                        }
                        extra |= self.bits.take(nb) as usize;
                        (1 << (nb + 1)) + 1 + extra
                    }
                    _ => {
                        self.err = Some(FlateError::Corrupt(self.bits.roffset));
                        return;
                    }
                };
                if dist > self.dict.hist_size() {
                    self.err = Some(FlateError::Corrupt(self.bits.roffset));
                    return;
                }
                self.copy_len = length;
                self.copy_dist = dist;
            }
            copying = false;

            let mut cnt = self.dict.try_write_copy(self.copy_dist, self.copy_len);
            if cnt == 0 {
                cnt = self.dict.write_copy(self.copy_dist, self.copy_len);
            }
            self.copy_len -= cnt;
            if self.dict.avail_write() == 0 || self.copy_len > 0 {
                self.to_read = self.dict.read_flush();
                self.step = Step::Huffman { copying: true };
                return;
            }
        }
    }

    /// Go's `dataBlock`: a stored block's header, then its bytes.
    fn data_block<R: BufRead>(&mut self, src: &mut R) {
        self.bits.nb = 0;
        self.bits.b = 0;
        let mut header = [0_u8; 4];
        let (read, err) = read_full(src, &mut header);
        self.bits.roffset += i64::try_from(read).unwrap_or_default();
        if let Some(err) = err {
            self.err = Some(err);
            return;
        }
        let [l0, l1, n0, n1] = header;
        let n = u16::from_le_bytes([l0, l1]);
        if u16::from_le_bytes([n0, n1]) != !n {
            self.err = Some(FlateError::Corrupt(self.bits.roffset));
            return;
        }
        if n == 0 {
            self.to_read = self.dict.read_flush();
            self.finish_block();
            return;
        }
        self.copy_len = usize::from(n);
        self.copy_data(src);
    }

    /// Go's `copyData`: stored bytes straight into the window, pausing when it fills.
    fn copy_data<R: BufRead>(&mut self, src: &mut R) {
        let start = self.dict.wr_pos;
        let len = self.dict.avail_write().min(self.copy_len);
        let (cnt, err) = match self.dict.hist.get_mut(start..start + len) {
            Some(slice) => read_full(src, slice),
            None => (0, None),
        };
        self.bits.roffset += i64::try_from(cnt).unwrap_or_default();
        self.copy_len -= cnt;
        self.dict.wr_pos += cnt;
        if let Some(err) = err {
            self.err = Some(err);
            return;
        }
        if self.dict.avail_write() == 0 || self.copy_len > 0 {
            self.to_read = self.dict.read_flush();
            self.step = Step::CopyData;
            return;
        }
        self.finish_block();
    }

    fn finish_block(&mut self) {
        if self.is_final {
            if self.dict.avail_read() > 0 {
                self.to_read = self.dict.read_flush();
            }
            self.err = Some(FlateError::End);
        }
        self.step = Step::NextBlock;
    }
}
