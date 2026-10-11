//! Boundary cases for the Go-compatible gzip reader. Every row of `cases` was fed to Go 1.27.1's
//! `gzip.NewReader` followed by a read loop, and `outcome` is what Go produced: whether `NewReader` failed, the
//! bytes the loop collected and how it ended.

use std::io::{self, ErrorKind, Read, Seek, Write};

use flate2::write::GzEncoder;
use flate2::{Compression, Crc};

use super::{FLAG_COMMENT, FLAG_EXTRA, FLAG_HCRC, FLAG_NAME, Reader};

/// How Go's error ended a read loop or a `NewReader`, by message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fail {
    /// `io.EOF`, only possible from `NewReader` on an empty file.
    Eof,
    /// `io.ErrUnexpectedEOF`.
    UnexpectedEof,
    /// `gzip.ErrHeader`.
    Header,
    /// `gzip.ErrChecksum`.
    Checksum,
    /// `flate.CorruptInputError` at this offset.
    Corrupt(i64),
}

impl Fail {
    fn matches(self, err: &io::Error) -> bool {
        let message = err.to_string();
        match self {
            Self::Eof => err.kind() == ErrorKind::UnexpectedEof && message == "EOF",
            Self::UnexpectedEof => {
                err.kind() == ErrorKind::UnexpectedEof && message == "unexpected EOF"
            }
            Self::Header => {
                err.kind() == ErrorKind::InvalidData && message == "gzip: invalid header"
            }
            Self::Checksum => {
                err.kind() == ErrorKind::InvalidData && message == "gzip: invalid checksum"
            }
            Self::Corrupt(offset) => {
                err.kind() == ErrorKind::InvalidData
                    && message == format!("flate: corrupt input before offset {offset}")
            }
        }
    }
}

#[derive(Debug)]
enum Outcome {
    /// `gzip.NewReader` returned this error.
    NewFails(Fail),
    /// `NewReader` succeeded; reading yielded `data`, then `end` (`None` is a clean `io.EOF`).
    Reads { data: Vec<u8>, end: Option<Fail> },
}

fn reads(data: &[u8], end: Option<Fail>) -> Outcome {
    Outcome::Reads {
        data: data.to_vec(),
        end,
    }
}

struct Case {
    name: &'static str,
    bytes: Vec<u8>,
    outcome: Outcome,
}

fn header(flags: u8) -> Vec<u8> {
    vec![0x1f, 0x8b, 8, flags, 0, 0, 0, 0, 0, 3]
}

/// A stored (type 0) DEFLATE block.
fn stored(last: bool, data: &[u8]) -> Vec<u8> {
    let len = u16::try_from(data.len()).unwrap();
    [
        &[u8::from(last)][..],
        &len.to_le_bytes(),
        &(!len).to_le_bytes(),
        data,
    ]
    .concat()
}

fn trailer(data: &[u8]) -> Vec<u8> {
    let mut crc = Crc::new();
    crc.update(data);
    [crc.sum().to_le_bytes(), crc.amount().to_le_bytes()].concat()
}

/// A member whose body is one final stored block holding `data`.
fn member(data: &[u8]) -> Vec<u8> {
    [header(0), stored(true, data), trailer(data)].concat()
}

/// A member whose body is the fixed-Huffman block for "a".
fn fixed_a() -> Vec<u8> {
    [header(0), vec![0x4b, 0x04, 0x00], trailer(b"a")].concat()
}

/// `header(flags)` followed by `fields`, then the one-block body and trailer for "a\n".
fn with_fields(flags: u8, fields: &[u8]) -> Vec<u8> {
    [
        header(flags),
        fields.to_vec(),
        stored(true, b"a\n"),
        trailer(b"a\n"),
    ]
    .concat()
}

/// Appends the low 16 bits of the CRC-32 of `head`, as FHCRC stores them.
fn with_header_crc(head: &[u8], flip: u8) -> Vec<u8> {
    let mut crc = Crc::new();
    crc.update(head);
    let [lo, hi, ..] = crc.sum().to_le_bytes();
    [
        head,
        &[lo ^ flip, hi],
        &stored(true, b"a\n"),
        &trailer(b"a\n"),
    ]
    .concat()
}

fn name_of(len: usize, terminated: bool) -> Vec<u8> {
    let mut name = vec![b'x'; len];
    if terminated {
        name.push(0);
    }
    name
}

/// Packs bits LSB first, the way DEFLATE streams are laid out.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    nbits: usize,
}

impl BitWriter {
    fn bit(&mut self, bit: bool) {
        if self.nbits.is_multiple_of(8) {
            self.bytes.push(0);
        }
        if bit {
            if let Some(last) = self.bytes.last_mut() {
                *last |= 1 << (self.nbits % 8);
            }
        }
        self.nbits += 1;
    }

    /// Header fields and extra bits: least significant bit first.
    fn value(&mut self, value: u32, n: u32) {
        for i in 0..n {
            self.bit((value >> i) & 1 == 1);
        }
    }

    /// Huffman codes: most significant bit first.
    fn code(&mut self, code: u32, n: u32) {
        for i in (0..n).rev() {
            self.bit((code >> i) & 1 == 1);
        }
    }
}

/// A single fixed-Huffman block built from literals and back references, with the bytes it must decode to.
struct FixedBlock {
    bits: BitWriter,
    out: Vec<u8>,
}

impl FixedBlock {
    fn new() -> Self {
        let mut bits = BitWriter::default();
        bits.value(1, 1);
        bits.value(1, 2);
        Self {
            bits,
            out: Vec::new(),
        }
    }

    fn symbol(&mut self, symbol: u32) {
        match symbol {
            0..144 => self.bits.code(0x30 + symbol, 8),
            144..256 => self.bits.code(0x190 + symbol - 144, 9),
            256..280 => self.bits.code(symbol - 256, 7),
            _ => self.bits.code(0xc0 + symbol - 280, 8),
        }
    }

    fn literals(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.symbol(u32::from(byte));
            self.out.push(byte);
        }
    }

    fn copy(&mut self, length: u32, dist: usize) {
        match length {
            3..=10 => self.symbol(254 + length),
            11..=18 => {
                self.symbol(265 + (length - 11) / 2);
                self.bits.value((length - 11) % 2, 1);
            }
            19..=34 => {
                self.symbol(269 + (length - 19) / 4);
                self.bits.value((length - 19) % 4, 2);
            }
            258 => self.symbol(285),
            _ => panic!("unsupported length {length}"),
        }
        let (code, nb, base) = (0..30_u32)
            .map(|code| {
                let nb = if code < 4 { 0 } else { (code - 2) / 2 };
                let base = if code < 4 {
                    code + 1
                } else {
                    (1 << (nb + 1)) + 1 + ((code & 1) << nb)
                };
                (code, nb, base as usize)
            })
            .rfind(|&(_, _, base)| base <= dist)
            .unwrap();
        self.bits.code(code, 5);
        self.bits.value(u32::try_from(dist - base).unwrap(), nb);
        for _ in 0..length {
            self.out.push(self.out[self.out.len() - dist]);
        }
    }

    /// The raw DEFLATE bytes, closed by the end-of-block symbol, and the decoded bytes.
    fn finish(mut self) -> (Vec<u8>, Vec<u8>) {
        self.symbol(256);
        (self.bits.bytes, self.out)
    }
}

/// A member with a fixed-Huffman body that uses short, extra-bit and maximal matches.
fn fixed_member() -> (Vec<u8>, Vec<u8>) {
    let mut block = FixedBlock::new();
    block.literals(b"abc\n");
    block.copy(3, 4);
    block.literals(b"xyz");
    block.copy(20, 7);
    block.copy(258, 1);
    block.copy(12, 9);
    block.copy(34, 300 - 130);
    let (body, out) = block.finish();
    ([header(0), body, trailer(&out)].concat(), out)
}

fn from_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

/// A member whose final block is a dynamic-Huffman block cut off inside its last symbols.
fn truncated_dynamic_member() -> Vec<u8> {
    from_hex(
        "1f8b080000000000000305c0010480000000a0ffffefff070018003cc000000000b079e099011b00000000000000000000000000000080bde3716ef773ecebf58ccb3c4dd3e71d8f73bb9f635faf675ce6699a3e",
    )
}

/// Back references may not reach before the member's own output, even into the previous member's window.
fn window_member_then_reach_back() -> Vec<u8> {
    let mut data = vec![b'\n'; 32748];
    data.extend_from_slice(b"{\"elapsed_ms\":1000}\n");
    assert_eq!(data.len(), 32768);
    [
        member(&data),
        from_hex("1f8b0800000000000003c3260600c18a580b14000000"),
    ]
    .concat()
}

/// A dynamic block header followed by `rest`: HLIT, HDIST and HCLEN, then 3-bit code length code lengths.
fn dynamic_header(
    hlit: u32,
    hdist: u32,
    code_lengths: &[u32],
    rest: impl FnOnce(&mut BitWriter),
) -> Vec<u8> {
    let mut bits = BitWriter::default();
    bits.value(1, 1);
    bits.value(2, 2);
    bits.value(hlit, 5);
    bits.value(hdist, 5);
    bits.value(u32::try_from(code_lengths.len() - 4).unwrap(), 4);
    for &len in code_lengths {
        bits.value(len, 3);
    }
    rest(&mut bits);
    [header(0), bits.bytes].concat()
}

fn cases() -> Vec<Case> {
    let a = b"a\n";
    let mut cases = Vec::new();
    let mut add = |name, bytes: Vec<u8>, outcome| {
        cases.push(Case {
            name,
            bytes,
            outcome,
        });
    };

    // Members.
    add("single member", member(a), reads(b"a\n", None));
    add("fixed huffman member", fixed_a(), reads(b"a", None));
    add(
        "two members",
        [member(a), member(b"b\n")].concat(),
        reads(b"a\nb\n", None),
    );
    add(
        "two fixed members share no bit state",
        [fixed_a(), fixed_a()].concat(),
        reads(b"aa", None),
    );
    add("empty member", member(b""), reads(b"", None));
    add(
        "empty member between members",
        [member(a), member(b""), member(b"b\n")].concat(),
        reads(b"a\nb\n", None),
    );

    // Header.
    add(
        "reserved flag bits and FTEXT ignored",
        with_fields(0xe1, &[]),
        reads(b"a\n", None),
    );
    add("empty file", Vec::new(), Outcome::NewFails(Fail::Eof));
    add(
        "one byte",
        vec![0x1f],
        Outcome::NewFails(Fail::UnexpectedEof),
    );
    add(
        "nine byte header",
        header(0)[..9].to_vec(),
        Outcome::NewFails(Fail::UnexpectedEof),
    );
    add(
        "bad ID1",
        [vec![0x1e], member(a)[1..].to_vec()].concat(),
        Outcome::NewFails(Fail::Header),
    );
    add(
        "bad ID2",
        [member(a)[..1].to_vec(), vec![0x8c], member(a)[2..].to_vec()].concat(),
        Outcome::NewFails(Fail::Header),
    );
    add(
        "bad CM",
        [member(a)[..2].to_vec(), vec![7], member(a)[3..].to_vec()].concat(),
        Outcome::NewFails(Fail::Header),
    );
    add(
        "not gzip",
        b"{\"elapsed_ms\":1000}\n".to_vec(),
        Outcome::NewFails(Fail::Header),
    );

    add(
        "FEXTRA of 300 bytes skipped",
        with_fields(
            FLAG_EXTRA,
            &[&300_u16.to_le_bytes()[..], &[7; 300]].concat(),
        ),
        reads(b"a\n", None),
    );
    add(
        "FEXTRA of 0 bytes",
        with_fields(FLAG_EXTRA, &[0, 0]),
        reads(b"a\n", None),
    );
    add(
        "FEXTRA of 65535 bytes",
        with_fields(FLAG_EXTRA, &[&[0xff, 0xff][..], &vec![7; 65535]].concat()),
        reads(b"a\n", None),
    );
    add(
        "FEXTRA length cut",
        [header(FLAG_EXTRA), vec![5]].concat(),
        Outcome::NewFails(Fail::UnexpectedEof),
    );
    add(
        "FEXTRA length only",
        [header(FLAG_EXTRA), vec![5, 0]].concat(),
        Outcome::NewFails(Fail::UnexpectedEof),
    );
    add(
        "FEXTRA data cut",
        [header(FLAG_EXTRA), vec![5, 0, 1, 2]].concat(),
        Outcome::NewFails(Fail::UnexpectedEof),
    );

    add(
        "FNAME of 511 bytes accepted",
        with_fields(FLAG_NAME, &name_of(511, true)),
        reads(b"a\n", None),
    );
    add(
        "FNAME of 512 bytes rejected",
        with_fields(FLAG_NAME, &name_of(512, true)),
        Outcome::NewFails(Fail::Header),
    );
    add(
        "FNAME of 512 bytes then EOF",
        [header(FLAG_NAME), name_of(512, false)].concat(),
        Outcome::NewFails(Fail::Header),
    );
    add(
        "FNAME of 511 bytes then EOF",
        [header(FLAG_NAME), name_of(511, false)].concat(),
        Outcome::NewFails(Fail::UnexpectedEof),
    );
    add(
        "FNAME empty",
        with_fields(FLAG_NAME, &[0]),
        reads(b"a\n", None),
    );
    add(
        "FNAME unterminated",
        [header(FLAG_NAME), b"abc".to_vec()].concat(),
        Outcome::NewFails(Fail::UnexpectedEof),
    );
    add(
        "FNAME missing",
        header(FLAG_NAME),
        Outcome::NewFails(Fail::UnexpectedEof),
    );

    add(
        "FCOMMENT of 511 bytes accepted",
        with_fields(FLAG_COMMENT, &name_of(511, true)),
        reads(b"a\n", None),
    );
    add(
        "FCOMMENT of 512 bytes rejected",
        with_fields(FLAG_COMMENT, &name_of(512, true)),
        Outcome::NewFails(Fail::Header),
    );
    add(
        "FCOMMENT of 512 bytes then EOF",
        [header(FLAG_COMMENT), name_of(512, false)].concat(),
        Outcome::NewFails(Fail::Header),
    );
    add(
        "FCOMMENT unterminated",
        [header(FLAG_COMMENT), b"abc".to_vec()].concat(),
        Outcome::NewFails(Fail::UnexpectedEof),
    );
    add(
        "FNAME and FCOMMENT of 511 bytes each",
        with_fields(
            FLAG_NAME | FLAG_COMMENT,
            &[name_of(511, true), name_of(511, true)].concat(),
        ),
        reads(b"a\n", None),
    );
    add(
        "FEXTRA FNAME FCOMMENT in order",
        with_fields(
            FLAG_EXTRA | FLAG_NAME | FLAG_COMMENT,
            &[&[2, 0, 9, 9][..], b"n\0", b"c\0"].concat(),
        ),
        reads(b"a\n", None),
    );

    add(
        "FHCRC correct",
        with_header_crc(&header(FLAG_HCRC), 0),
        reads(b"a\n", None),
    );
    add(
        "FHCRC wrong",
        with_header_crc(&header(FLAG_HCRC), 1),
        Outcome::NewFails(Fail::Header),
    );
    add(
        "FHCRC covers FNAME",
        with_header_crc(
            &[header(FLAG_NAME | FLAG_HCRC), b"file\0".to_vec()].concat(),
            0,
        ),
        reads(b"a\n", None),
    );
    add(
        "FHCRC covers FEXTRA FNAME FCOMMENT",
        with_header_crc(
            &[
                header(FLAG_EXTRA | FLAG_NAME | FLAG_COMMENT | FLAG_HCRC),
                vec![1, 0, 9],
                b"n\0".to_vec(),
                b"c\0".to_vec(),
            ]
            .concat(),
            0,
        ),
        reads(b"a\n", None),
    );
    add(
        "FHCRC over header without the stored bytes",
        with_header_crc(
            &[header(FLAG_NAME | FLAG_HCRC), b"file\0".to_vec()].concat(),
            1,
        ),
        Outcome::NewFails(Fail::Header),
    );
    add(
        "FHCRC cut",
        [header(FLAG_HCRC), vec![0]].concat(),
        Outcome::NewFails(Fail::UnexpectedEof),
    );
    add(
        "FHCRC missing",
        header(FLAG_HCRC),
        Outcome::NewFails(Fail::UnexpectedEof),
    );

    // After a member.
    add(
        "trailing byte",
        [member(a), vec![0]].concat(),
        reads(b"a\n", Some(Fail::UnexpectedEof)),
    );
    add(
        "trailing nine bytes",
        [member(a), vec![0x1f; 9]].concat(),
        reads(b"a\n", Some(Fail::UnexpectedEof)),
    );
    add(
        "trailing garbage",
        [member(a), vec![0; 10]].concat(),
        reads(b"a\n", Some(Fail::Header)),
    );
    add(
        "trailing text",
        [member(a), b"not gzip at all".to_vec()].concat(),
        reads(b"a\n", Some(Fail::Header)),
    );
    add(
        "second header bad hcrc",
        [member(a), with_header_crc(&header(FLAG_HCRC), 1)].concat(),
        reads(b"a\n", Some(Fail::Header)),
    );
    add(
        "second header cut",
        [member(a), header(FLAG_NAME), b"n".to_vec()].concat(),
        reads(b"a\n", Some(Fail::UnexpectedEof)),
    );
    add(
        "second member truncated body",
        [member(a), header(0), stored(false, b"b\n")].concat(),
        reads(b"a\nb\n", Some(Fail::UnexpectedEof)),
    );

    // Checksums.
    let mut bad_crc = member(a);
    let len = bad_crc.len();
    bad_crc[len - 8] ^= 1;
    add(
        "bad CRC",
        bad_crc.clone(),
        reads(b"a\n", Some(Fail::Checksum)),
    );
    add(
        "bad CRC stops before next member",
        [bad_crc, member(b"b\n")].concat(),
        reads(b"a\n", Some(Fail::Checksum)),
    );
    let mut bad_size = member(a);
    let len = bad_size.len();
    bad_size[len - 4] ^= 1;
    add("bad ISIZE", bad_size, reads(b"a\n", Some(Fail::Checksum)));
    let mut bad_size_high = member(a);
    let len = bad_size_high.len();
    bad_size_high[len - 1] ^= 0x80;
    add(
        "bad ISIZE high byte",
        bad_size_high,
        reads(b"a\n", Some(Fail::Checksum)),
    );
    let whole = member(a);
    add(
        "trailer cut to seven bytes",
        whole[..whole.len() - 1].to_vec(),
        reads(b"a\n", Some(Fail::UnexpectedEof)),
    );
    add(
        "trailer cut to four bytes",
        whole[..whole.len() - 4].to_vec(),
        reads(b"a\n", Some(Fail::UnexpectedEof)),
    );
    add(
        "trailer missing",
        whole[..whole.len() - 8].to_vec(),
        reads(b"a\n", Some(Fail::UnexpectedEof)),
    );

    // Bodies.
    add(
        "body missing",
        header(0),
        reads(b"", Some(Fail::UnexpectedEof)),
    );
    add(
        "body cut in block header",
        [header(0), vec![1]].concat(),
        reads(b"", Some(Fail::UnexpectedEof)),
    );
    add(
        "body cut in stored length",
        [header(0), vec![1, 2, 0, 0xfd]].concat(),
        reads(b"", Some(Fail::UnexpectedEof)),
    );
    add(
        "non-final stored block then EOF",
        [header(0), stored(false, a)].concat(),
        reads(b"a\n", Some(Fail::UnexpectedEof)),
    );
    add(
        "stored data cut short",
        [
            header(0),
            stored(false, a),
            stored(true, b"bcdef")[..7].to_vec(),
        ]
        .concat(),
        reads(b"a\nbc", Some(Fail::UnexpectedEof)),
    );
    add(
        "fixed block cut",
        [header(0), vec![0x4b]].concat(),
        reads(b"", Some(Fail::UnexpectedEof)),
    );
    add(
        "stored block then reserved block type",
        [header(0), stored(false, a), vec![0x06, 0, 0, 0]].concat(),
        reads(b"a\n", Some(Fail::Corrupt(8))),
    );
    add(
        "stored block then reserved block type at EOF",
        [header(0), stored(false, a), vec![0x06]].concat(),
        reads(b"a\n", Some(Fail::Corrupt(8))),
    );
    add(
        "two stored blocks then reserved block type",
        [
            header(0),
            stored(false, a),
            stored(false, b"b\n"),
            vec![0x06, 0, 0, 0],
        ]
        .concat(),
        reads(b"a\nb\n", Some(Fail::Corrupt(15))),
    );
    add(
        "stored length complement mismatch",
        [
            header(0),
            stored(false, a),
            vec![0x01, 2, 0, 0, 0],
            a.to_vec(),
        ]
        .concat(),
        reads(b"a\n", Some(Fail::Corrupt(12))),
    );
    add(
        "reserved block type first",
        [header(0), vec![0x06, 0, 0, 0]].concat(),
        reads(b"", Some(Fail::Corrupt(1))),
    );
    add(
        "corrupt second member keeps first member's data",
        [member(a), header(0), vec![0x06, 0, 0, 0]].concat(),
        reads(b"a\n", Some(Fail::Corrupt(1))),
    );

    // Damaged DEFLATE bodies, with Go's input offsets.
    add(
        "raw body 03 02 00 reaches before the history",
        [header(0), vec![0x03, 0x02, 0x00], trailer(&[0; 3])].concat(),
        reads(b"", Some(Fail::Corrupt(2))),
    );
    add(
        "back reference into the previous member",
        window_member_then_reach_back(),
        reads(
            &[b"\n".repeat(32748), b"{\"elapsed_ms\":1000}\n".to_vec()].concat(),
            Some(Fail::Corrupt(3)),
        ),
    );
    add(
        "fixed block reference past the history",
        {
            let mut block = FixedBlock::new();
            block.literals(b"a");
            block.symbol(257);
            block.bits.code(1, 5);
            [header(0), block.bits.bytes].concat()
        },
        reads(b"a", Some(Fail::Corrupt(3))),
    );
    add(
        "fixed block length symbol 286",
        {
            let mut block = FixedBlock::new();
            block.literals(b"ab");
            block.symbol(286);
            [header(0), block.bits.bytes].concat()
        },
        reads(b"ab", Some(Fail::Corrupt(4))),
    );
    add(
        "fixed block distance code 30",
        {
            let mut block = FixedBlock::new();
            block.literals(b"ab");
            block.symbol(257);
            block.bits.code(30, 5);
            [header(0), block.bits.bytes].concat()
        },
        reads(b"ab", Some(Fail::Corrupt(4))),
    );
    add(
        "fixed block distance 1 before one byte",
        {
            let mut block = FixedBlock::new();
            block.literals(b"a");
            block.copy(3, 1);
            let (body, out) = block.finish();
            [header(0), body, trailer(&out)].concat()
        },
        reads(b"aaaa", None),
    );
    add(
        "dynamic block HLIT 288",
        dynamic_header(31, 0, &[0; 4], |_| {}),
        reads(b"", Some(Fail::Corrupt(3))),
    );
    add(
        "dynamic block HDIST 31",
        dynamic_header(0, 30, &[0; 4], |_| {}),
        reads(b"", Some(Fail::Corrupt(3))),
    );
    add(
        "dynamic block empty code length code",
        dynamic_header(0, 0, &[0; 4], |bits| bits.value(0, 8)),
        reads(b"", Some(Fail::Corrupt(4))),
    );
    add(
        "dynamic block repeat with no previous length",
        dynamic_header(0, 0, &[1, 0, 0, 1], |bits| {
            bits.code(1, 1);
            bits.value(0, 2);
        }),
        reads(b"", Some(Fail::Corrupt(4))),
    );
    add(
        "dynamic block oversubscribed code length code",
        dynamic_header(0, 0, &[1, 1, 1, 0], |_| {}),
        reads(b"", Some(Fail::Corrupt(4))),
    );
    add(
        "truncated dynamic member",
        truncated_dynamic_member(),
        reads(
            b"{\"elapsed_ms\":1000}\n{\"elapsed_ms\":1000}",
            Some(Fail::UnexpectedEof),
        ),
    );
    add(
        "fixed member",
        fixed_member().0,
        reads(&fixed_member().1, None),
    );

    cases
}

fn open(bytes: &[u8]) -> io::Result<Reader> {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file.rewind().unwrap();
    Reader::new(file)
}

/// Reads with a `chunk`-sized buffer until the first error or EOF, like the read loop run against Go.
fn drain(reader: &mut Reader, chunk: usize) -> (Vec<u8>, Option<io::Error>) {
    let mut data = Vec::new();
    let mut buf = vec![0; chunk];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return (data, None),
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(err) => return (data, Some(err)),
        }
    }
}

fn check(case: &Case, chunk: usize) {
    let name = case.name;
    match (&case.outcome, open(&case.bytes)) {
        (Outcome::NewFails(want), Err(err)) => {
            assert!(
                want.matches(&err),
                "{name}: new failed with {:?} {err}, want {want:?}",
                err.kind()
            );
        }
        (Outcome::NewFails(want), Ok(_)) => panic!("{name}: new succeeded, want {want:?}"),
        (Outcome::Reads { .. }, Err(err)) => panic!("{name}: new failed: {err}"),
        (Outcome::Reads { data, end }, Ok(mut reader)) => {
            let (got, err) = drain(&mut reader, chunk);
            assert_eq!(got, *data, "{name}: bytes with {chunk}-byte reads");
            match (end, &err) {
                (None, None) => {
                    // Clean EOF stays EOF.
                    assert_eq!(
                        reader.read(&mut [0; 8]).unwrap(),
                        0,
                        "{name}: read after EOF"
                    );
                }
                (Some(want), Some(err)) => {
                    assert!(
                        want.matches(err),
                        "{name}: read failed with {:?} {err}, want {want:?}",
                        err.kind()
                    );
                    // Errors are sticky, as Go keeps `z.err`.
                    let again = reader.read(&mut [0; 8]).unwrap_err();
                    assert_eq!(
                        (again.kind(), again.to_string()),
                        (err.kind(), err.to_string()),
                        "{name}: second read"
                    );
                    let (more, last) = drain(&mut reader, chunk);
                    assert!(more.is_empty(), "{name}: bytes after the error");
                    assert_eq!(
                        last.map(|e| e.to_string()),
                        Some(err.to_string()),
                        "{name}: third read"
                    );
                }
                (want, got) => panic!("{name}: read ended with {got:?}, want {want:?}"),
            }
        }
    }
}

#[test]
fn go_boundary_cases() {
    for case in &cases() {
        check(case, 4096);
    }
}

#[test]
fn go_boundary_cases_with_one_byte_reads() {
    for case in &cases() {
        check(case, 1);
    }
}

#[test]
fn go_boundary_cases_with_odd_reads() {
    for case in &cases() {
        check(case, 3);
    }
}

#[test]
fn zero_length_read_returns_zero() {
    let mut reader = open(&member(b"a\n")).unwrap();
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    assert_eq!(drain(&mut reader, 8).0, b"a\n");
}

/// Deterministic, compressible text longer than the 32 KiB window, with back references across it.
fn long_text(lines: usize) -> Vec<u8> {
    let mut state = 12_345_u32;
    let mut text = Vec::new();
    for i in 0..lines {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        text.extend_from_slice(
            format!("{{\"elapsed_ms\":{i},\"v\":{}}}\n", state % 1000).as_bytes(),
        );
    }
    text
}

fn gzip_member(data: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn compressed_members_larger_than_the_window_round_trip() {
    let first = long_text(9000);
    let second = long_text(4000);
    assert!(first.len() > 3 * 32_768);
    let bytes = [gzip_member(&first), gzip_member(&second)].concat();
    for chunk in [1, 7, 4096, 100_000] {
        let mut reader = open(&bytes).unwrap();
        let (got, err) = drain(&mut reader, chunk);
        assert!(err.is_none(), "{chunk}-byte reads: {err:?}");
        assert_eq!(
            got,
            [first.clone(), second.clone()].concat(),
            "{chunk}-byte reads"
        );
    }
}

#[test]
fn truncated_compressed_member_yields_a_prefix_then_unexpected_eof() {
    let text = long_text(9000);
    let whole = gzip_member(&text);
    let cut = whole[..whole.len() / 2].to_vec();
    let mut reader = open(&cut).unwrap();
    let (got, err) = drain(&mut reader, 4096);
    assert!(!got.is_empty() && got.len() < text.len());
    assert_eq!(got, text[..got.len()]);
    assert!(Fail::UnexpectedEof.matches(&err.unwrap()));
}

#[test]
fn corrupt_compressed_member_ends_in_invalid_data() {
    let text = long_text(9000);
    let mut bytes = gzip_member(&text);
    // A flipped byte mid-body decodes to different bytes until the inflater or the checksum notices.
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    let mut reader = open(&bytes).unwrap();
    let (_, err) = drain(&mut reader, 4096);
    let err = err.expect("corruption must be reported");
    assert_eq!(err.kind(), ErrorKind::InvalidData, "{err}");
}

/// Go's byte count for every cut of `member` from its 10-byte header on: the reader must hand out exactly that many
/// bytes of `expected`, then fail with an unexpected EOF.
fn check_cuts(member: &[u8], expected: &[u8], counts: &[usize]) {
    for (i, &want) in counts.iter().enumerate() {
        let cut = 10 + i;
        for chunk in [4096, 1] {
            let mut reader = open(&member[..cut]).unwrap();
            let (got, err) = drain(&mut reader, chunk);
            assert_eq!(got.len(), want, "cut at {cut}, {chunk}-byte reads");
            assert_eq!(got, expected[..want], "cut at {cut}, {chunk}-byte reads");
            let err = err.unwrap();
            assert!(Fail::UnexpectedEof.matches(&err), "cut at {cut}: {err}");
        }
    }
}

/// Go's byte counts for the cuts of the fixed member.
const FIXED_CUTS: [usize; 26] = [
    0, 0, 1, 2, 3, 4, 7, 8, 9, 10, 10, 30, 30, 288, 288, 300, 300, 334, 334, 334, 334, 334, 334,
    334, 334, 334,
];

#[test]
fn fixed_block_truncations_match_go_byte_counts() {
    let (member, out) = fixed_member();
    assert_eq!(out.len(), 334);
    // Cuts 10 up to, not including, the whole member.
    assert_eq!(10 + FIXED_CUTS.len(), member.len());
    check_cuts(&member, &out, &FIXED_CUTS);
}

/// Go's byte counts for the cuts of the dynamic member.
const DYNAMIC_CUTS: [usize; 75] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 5, 6, 7, 9, 10, 11, 13, 14, 15, 17, 18, 19,
    21, 23, 24, 25, 27, 28, 29, 31, 32, 33, 35, 36, 37, 39,
];

#[test]
fn dynamic_block_truncations_match_go_byte_counts() {
    // Cuts 10 up to and including the whole member, which is itself cut inside the block: Go stops after 39 bytes
    // where a decoder reading a 1-bit literal from the last bits would produce a 40th.
    let member = truncated_dynamic_member();
    let expected = b"{\"elapsed_ms\":1000}\n{\"elapsed_ms\":1000}";
    assert_eq!(9 + DYNAMIC_CUTS.len(), member.len());
    check_cuts(&member, expected, &DYNAMIC_CUTS);
}
