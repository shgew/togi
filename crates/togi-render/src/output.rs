use std::ffi::OsStr;
use std::io::IsTerminal as _;
use std::os::fd::AsFd;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OutputFacts {
    pub is_terminal: bool,
    pub no_color: bool,
    pub journald: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StreamIdentity {
    device: u64,
    inode: u64,
}

fn parse_decimal(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_journal_stream(value: &OsStr) -> Option<StreamIdentity> {
    let (device, inode) = value.to_str()?.split_once(':')?;
    Some(StreamIdentity {
        device: parse_decimal(device)?,
        inode: parse_decimal(inode)?,
    })
}

fn widen<T: TryInto<u64>>(value: T) -> Option<u64> {
    value.try_into().ok()
}

fn identity_of(stream: &impl AsFd) -> Option<StreamIdentity> {
    let stat = rustix::fs::fstat(stream).ok()?;
    Some(StreamIdentity {
        device: widen(stat.st_dev)?,
        inode: widen(stat.st_ino)?,
    })
}

impl OutputFacts {
    #[must_use]
    pub fn detect(
        stream: &impl AsFd,
        no_color: Option<&OsStr>,
        journal_stream: Option<&OsStr>,
    ) -> Self {
        Self::from_parts(
            stream.as_fd().is_terminal(),
            identity_of(stream),
            no_color,
            journal_stream,
        )
    }

    /// A stream without a readable identity (Darwin's `dev_t` is signed, and devfs device numbers can be
    /// negative) is still a terminal when isatty says so; it only cannot be the journald stream.
    fn from_parts(
        is_terminal: bool,
        identity: Option<StreamIdentity>,
        no_color: Option<&OsStr>,
        journal_stream: Option<&OsStr>,
    ) -> Self {
        Self {
            is_terminal,
            no_color: no_color.is_some_and(|value| !value.is_empty()),
            journald: identity.is_some_and(|identity| {
                journal_stream
                    .and_then(parse_journal_stream)
                    .is_some_and(|named| named == identity)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;

    use test_case::test_case;

    use super::{OutputFacts, StreamIdentity, parse_journal_stream};

    fn parse(value: &str) -> Option<StreamIdentity> {
        parse_journal_stream(OsStr::new(value))
    }

    fn identity_string(file: &File) -> String {
        let meta = file.metadata().unwrap();
        format!("{}:{}", meta.dev(), meta.ino())
    }

    #[test]
    fn journal_stream_parses_numeric_device_and_inode() {
        assert_eq!(
            parse("8:12345"),
            Some(StreamIdentity {
                device: 8,
                inode: 12345
            })
        );
        assert_eq!(
            parse("0:0"),
            Some(StreamIdentity {
                device: 0,
                inode: 0
            })
        );
        assert_eq!(
            parse("18446744073709551615:18446744073709551615"),
            Some(StreamIdentity {
                device: u64::MAX,
                inode: u64::MAX
            })
        );
        assert_eq!(
            parse("008:0012"),
            Some(StreamIdentity {
                device: 8,
                inode: 12
            })
        );
    }

    #[test_case(""; "empty")]
    #[test_case(":"; "colon only")]
    #[test_case("8"; "device only")]
    #[test_case("8:"; "missing inode")]
    #[test_case(":8"; "missing device")]
    #[test_case("8:9:10"; "three fields")]
    #[test_case("a:1"; "letter device")]
    #[test_case("1:b"; "letter inode")]
    #[test_case("-1:2"; "negative device")]
    #[test_case("1:-2"; "negative inode")]
    #[test_case("+1:2"; "signed device")]
    #[test_case("1:+2"; "signed inode")]
    #[test_case(" 1:2"; "leading space")]
    #[test_case("1: 2"; "space before inode")]
    #[test_case("1:2 "; "trailing space")]
    #[test_case("18446744073709551616:1"; "device overflow")]
    #[test_case("1:18446744073709551616"; "inode overflow")]
    #[test_case("0x8:1"; "hex device")]
    fn malformed_journal_streams_name_nothing(value: &str) {
        assert_eq!(parse(value), None);
    }

    #[test]
    fn a_journal_stream_that_is_not_utf8_names_nothing() {
        assert_eq!(parse_journal_stream(OsStr::from_bytes(b"1:\xff")), None);
    }

    #[test]
    fn real_fs_a_pseudo_terminal_is_a_terminal() {
        use std::os::unix::fs::OpenOptionsExt as _;

        let flags = rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY;
        let master = rustix::pty::openpt(flags).unwrap();
        rustix::pty::grantpt(&master).unwrap();
        rustix::pty::unlockpt(&master).unwrap();
        let slave_path = rustix::pty::ptsname(&master, Vec::new()).unwrap();
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(rustix::fs::OFlags::NOCTTY.bits().cast_signed())
            .open(OsStr::from_bytes(slave_path.as_bytes()))
            .unwrap();
        let facts = OutputFacts::detect(&slave, None, None);
        drop(master);
        assert_eq!(
            facts,
            OutputFacts {
                is_terminal: true,
                no_color: false,
                journald: false
            }
        );
    }

    #[test]
    fn a_terminal_without_a_readable_identity_is_still_a_terminal_but_never_journald() {
        let facts = OutputFacts::from_parts(true, None, None, Some(OsStr::new("8:12345")));
        assert_eq!(
            facts,
            OutputFacts {
                is_terminal: true,
                no_color: false,
                journald: false
            }
        );
    }

    #[test]
    fn real_fs_a_regular_file_is_plain_and_its_identity_is_detected() {
        let file = tempfile::tempfile().unwrap();
        assert_eq!(
            OutputFacts::detect(&file, None, None),
            OutputFacts::default()
        );
        let named = identity_string(&file);
        assert!(OutputFacts::detect(&file, None, Some(OsStr::new(&named))).journald);
    }

    #[test]
    fn real_fs_a_pipe_is_plain() {
        let (_reader, writer) = std::io::pipe().unwrap();
        assert_eq!(
            OutputFacts::detect(&writer, None, None),
            OutputFacts::default()
        );
    }

    #[test]
    fn real_fs_a_character_device_that_is_not_a_terminal_is_plain() {
        let null = File::options().write(true).open("/dev/null").unwrap();
        assert_eq!(
            OutputFacts::detect(&null, None, None),
            OutputFacts::default()
        );
    }

    #[test]
    fn journal_stream_matches_by_device_and_inode_not_file_type() {
        let identity = StreamIdentity {
            device: 8,
            inode: 12345,
        };
        let facts =
            OutputFacts::from_parts(false, Some(identity), None, Some(OsStr::new("8:12345")));
        assert_eq!(
            facts,
            OutputFacts {
                is_terminal: false,
                no_color: false,
                journald: true
            }
        );
    }

    #[test_case("8:12346"; "other inode")]
    #[test_case("9:12345"; "other device")]
    #[test_case("8:12345:1"; "extra field")]
    #[test_case("8:"; "missing inode")]
    #[test_case("garbage"; "not a stream")]
    fn a_journal_stream_naming_another_stream_is_ignored(wrong: &str) {
        let identity = StreamIdentity {
            device: 8,
            inode: 12345,
        };
        assert!(
            !OutputFacts::from_parts(false, Some(identity), None, Some(OsStr::new(wrong))).journald
        );
    }

    #[test]
    fn no_color_counts_only_when_non_empty() {
        assert!(OutputFacts::from_parts(false, None, Some(OsStr::new("1")), None).no_color);
        assert!(OutputFacts::from_parts(false, None, Some(OsStr::new("anything")), None).no_color);
        assert!(!OutputFacts::from_parts(false, None, Some(OsStr::new("")), None).no_color);
        assert!(!OutputFacts::from_parts(false, None, None, None).no_color);
    }
}
