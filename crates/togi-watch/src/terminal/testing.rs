//! Test support: a terminal backend that records what the loop does to it, and journals on disk.

use std::cell::{Cell as StdCell, RefCell};
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;

use jiff::Timestamp;
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use togi_journal::{Build, Event, Payload, SCHEMA, SessionId, SessionStart, SessionWarning, Stamp};
use togi_machine::{BootId, CcdId, CoreId, CoreInfo, CpuId};

/// One thing the loop did to the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Op {
    /// Raw bytes, as the palette sequences are written.
    Raw(String),
    /// The cells written: column, row and symbol.
    Draw(Vec<(u16, u16, String)>),
    HideCursor,
    ShowCursor,
    Clear(String),
    MoveCursor(u16, u16),
    Flush,
}

/// What a recording backend is told to fail at.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Faults {
    /// Fail the size read with this 1-based number, and every one after it.
    pub(super) size_from: Option<usize>,
    /// Fail the draw with this 1-based number, and every one after it.
    pub(super) draw_from: Option<usize>,
    /// Fail the first screen clear.
    pub(super) clear: bool,
}

/// The shared view of a recording backend, kept by the test.
#[derive(Debug, Clone)]
pub(super) struct Recorder {
    ops: Rc<RefCell<Vec<Op>>>,
    size: Rc<StdCell<Size>>,
    faults: Rc<StdCell<Faults>>,
    size_reads: Rc<StdCell<usize>>,
    draws: Rc<StdCell<usize>>,
    clear_calls: Rc<StdCell<usize>>,
}

impl Recorder {
    pub(super) fn new(width: u16, height: u16) -> Self {
        Self {
            ops: Rc::default(),
            size: Rc::new(StdCell::new(Size::new(width, height))),
            faults: Rc::default(),
            size_reads: Rc::default(),
            draws: Rc::default(),
            clear_calls: Rc::default(),
        }
    }

    pub(super) fn failing(width: u16, height: u16, faults: Faults) -> Self {
        let recorder = Self::new(width, height);
        recorder.faults.set(faults);
        recorder
    }

    pub(super) fn backend(&self) -> Recording {
        Recording(self.clone())
    }

    pub(super) fn resize(&self, width: u16, height: u16) {
        self.size.set(Size::new(width, height));
    }

    pub(super) fn ops(&self) -> Vec<Op> {
        self.ops.borrow().clone()
    }

    pub(super) fn clears(&self) -> usize {
        self.ops
            .borrow()
            .iter()
            .filter(|op| matches!(op, Op::Clear(kind) if kind == "All"))
            .count()
    }

    /// The cells of every draw so far, one list per draw.
    pub(super) fn draws(&self) -> Vec<Vec<(u16, u16, String)>> {
        self.ops
            .borrow()
            .iter()
            .filter_map(|op| match op {
                Op::Draw(cells) => Some(cells.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every raw write, in order.
    pub(super) fn raw(&self) -> Vec<String> {
        self.ops
            .borrow()
            .iter()
            .filter_map(|op| match op {
                Op::Raw(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn push(&self, op: Op) {
        self.ops.borrow_mut().push(op);
    }

    /// The terminal is back as the viewer had it: the screen cleared and homed, the cursor shown again, last.
    pub(super) fn restored(&self) -> bool {
        let ops = self.ops.borrow();
        let tail: Vec<_> = ops
            .iter()
            .rev()
            .filter(|op| !matches!(op, Op::Flush | Op::Raw(_)))
            .take(3)
            .collect();
        matches!(
            tail.as_slice(),
            [Op::ShowCursor, Op::MoveCursor(0, 0), Op::Clear(kind)] if kind == "All"
        )
    }
}

/// The backend the loop draws on.
pub(super) struct Recording(Recorder);

impl Write for Recording {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .push(Op::Raw(String::from_utf8_lossy(buf).into_owned()));
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn failure() -> io::Error {
    io::Error::other("terminal unavailable")
}

impl Backend for Recording {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let number = self.0.draws.get() + 1;
        self.0.draws.set(number);
        if self
            .0
            .faults
            .get()
            .draw_from
            .is_some_and(|from| number >= from)
        {
            return Err(failure());
        }
        let cells = content
            .map(|(x, y, cell)| (x, y, cell.symbol().to_owned()))
            .collect();
        self.0.push(Op::Draw(cells));
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.0.push(Op::HideCursor);
        Ok(())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.0.push(Op::ShowCursor);
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(Position::ORIGIN)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        self.0.push(Op::MoveCursor(position.x, position.y));
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        self.clear_region(ClearType::All)
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        let call = self.0.clear_calls.get() + 1;
        self.0.clear_calls.set(call);
        if self.0.faults.get().clear && call == 1 {
            return Err(failure());
        }
        self.0.push(Op::Clear(format!("{clear_type:?}")));
        Ok(())
    }

    fn size(&self) -> io::Result<Size> {
        let number = self.0.size_reads.get() + 1;
        self.0.size_reads.set(number);
        if self
            .0
            .faults
            .get()
            .size_from
            .is_some_and(|from| number >= from)
        {
            return Err(failure());
        }
        Ok(self.0.size.get())
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.0.size.get(),
            pixels: Size::new(0, 0),
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.push(Op::Flush);
        Ok(())
    }
}

const BOOT: &str = "boot-1";

pub(super) type TestEvent = Event;

fn event(seq: u64, payload: Payload) -> Event {
    Event::new(
        togi_journal::Seq::new(seq).unwrap(),
        Stamp {
            time: Timestamp::from_second(1_700_000_000 + i64::try_from(seq).unwrap()).unwrap(),
            mono: None,
        },
        BootId::from(BOOT.to_owned()),
        Vec::new(),
        payload,
    )
}

pub(super) fn session_start(seq: u64) -> Event {
    event(
        seq,
        Payload::SessionStart(SessionStart {
            build: Build {
                version: "0.1.0".into(),
                rev: None,
                ruleset: 1,
                schema: SCHEMA,
                fixes: 0,
                evidence: 1,
            },
            session: "20231114T221320Z".parse::<SessionId>().unwrap(),
            cores: vec![CoreInfo {
                core: CoreId::new(0).unwrap(),
                ccd: CcdId(0),
                cpus: vec![CpuId(0)],
            }],
        }),
    )
}

/// A session warning carrying `text`, which the log lists verbatim.
pub(super) fn warning(seq: u64, text: &str) -> Event {
    let mut warning = event(
        seq,
        Payload::SessionWarning(SessionWarning {
            operation: "write state projection".into(),
            trial: None,
            error: text.into(),
        }),
    );
    warning.msg = text.into();
    warning
}

/// The journal lines of `events`.
pub(super) fn lines(events: &[Event]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for event in events {
        serde_json::to_writer(&mut bytes, event).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

/// A session start followed by warnings "w2" to "w{n+1}" (sequence numbers 2 to n+1).
pub(super) fn session_with_warnings(warnings: u64) -> Vec<Event> {
    let mut events = vec![session_start(1)];
    for seq in 2..2 + warnings {
        events.push(warning(seq, &format!("w{seq}")));
    }
    events
}

/// Writes `events` as the journal in `directory` and gives it a fixed modification time.
pub(super) fn write_journal(directory: &Path, events: &[Event]) {
    write_bytes(directory, &lines(events));
}

pub(super) fn write_bytes(directory: &Path, bytes: &[u8]) {
    let path = directory.join("events.jsonl");
    std::fs::write(&path, bytes).unwrap();
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(2000))
        .unwrap();
}

/// A journal of this build whose second line does not decode: a problem to show, and to retry when it changes.
pub(super) fn undecodable_journal() -> Vec<u8> {
    let mut bytes = lines(&[session_start(1)]);
    bytes.extend_from_slice(b"not json\n");
    bytes
}

/// The first line of a Go build's journal, which has no `data` or `build` object, and a line after it that nothing
/// may read.
pub(super) fn go_journal() -> Vec<u8> {
    br#"{"seq":1,"time":"2026-09-27T22:19:54Z","boot":"b","kind":"session.start","msg":"m","ruleset":3,"schema":2,"session":"20260927T221954Z","cores":[]}
not json
"#
    .to_vec()
}
