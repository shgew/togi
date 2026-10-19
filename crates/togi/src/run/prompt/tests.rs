use std::cell::RefCell;
use std::io::{self, Write};
use std::os::fd::AsFd as _;
use std::rc::Rc;

use rustix::io::{Errno, write};
use test_case::test_case;
use togi_defect::FindingView;
use togi_journal::{DefectDirection, DefectId, Seq};
use togi_machine::{CancellationToken, CoreId, CoreSet};
use togi_render::open_pty;
use togi_session::PromptError;

use super::{Answers, ask, parse_answer, read_line, readable};

#[test_case("Yes \n", true ; "yes with a trailing space")]
#[test_case("Y\n", true ; "capital y")]
#[test_case("yEs\n", true ; "mixed case yes")]
#[test_case("no\n", false ; "no")]
#[test_case("sure\n", false ; "sure")]
#[test_case("\n", false ; "empty line")]
#[test_case("yes", false ; "incomplete line")]
#[test_case("", false ; "end of input")]
fn only_a_complete_line_saying_y_or_yes_is_a_yes(line: &str, want: bool) {
    assert_eq!(parse_answer(line), want);
}

#[derive(Clone, Default)]
struct Shared(Rc<RefCell<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).unwrap()
    }
}

struct Typed {
    out: Shared,
    line: Option<&'static str>,
}

impl Answers for Typed {
    fn line(&mut self, _: &CancellationToken) -> io::Result<Option<String>> {
        Ok(self.line.map(str::to_owned))
    }

    fn discard(&mut self) {
        self.out.0.borrow_mut().extend_from_slice(b"discarded\n");
    }
}

fn finding<'a>(cores: &[u8], decisions: &'a [Seq]) -> FindingView<'a> {
    let mut set = CoreSet::default();
    for core in cores {
        set.insert(CoreId::new(i64::from(*core)).unwrap());
    }
    FindingView {
        id: DefectId::new(2).unwrap(),
        entry: None,
        seq: Seq::FIRST,
        title: "t",
        detail: "",
        pr: 9,
        direction: DefectDirection::TooCautious,
        cores: set,
        decisions,
    }
}

#[test]
fn only_input_typed_after_the_question_is_the_answer() {
    let out = Shared::default();
    let mut answers = Typed {
        out: out.clone(),
        line: Some("y\n"),
    };
    let decisions = [];
    let yes = ask(
        &mut out.clone(),
        &mut answers,
        &CancellationToken::new(),
        &finding(&[3], &decisions),
    )
    .unwrap();
    assert!(yes);
    let text = out.text();
    assert!(text.starts_with("discarded\ntogi: defect 2:"), "{text:?}");
    assert!(text.ends_with("Reset cores 3? [y/N] "), "{text:?}");
}

#[test_case(Some("n\n") ; "no")]
#[test_case(None ; "end of input")]
fn the_question_names_every_core_and_a_no_or_end_of_input_is_no(line: Option<&'static str>) {
    let out = Shared::default();
    let mut answers = Typed {
        out: out.clone(),
        line,
    };
    let decisions = [];
    let yes = ask(
        &mut out.clone(),
        &mut answers,
        &CancellationToken::new(),
        &finding(&[3, 7], &decisions),
    )
    .unwrap();
    assert!(!yes);
    assert!(out.text().ends_with("Reset cores 3, 7? [y/N] "));
}

#[test]
fn a_run_stopped_while_the_question_is_open_ends_the_wait_without_an_answer() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut out = Shared::default();
    let mut answers = Typed {
        out: out.clone(),
        line: None,
    };
    let decisions = [];
    let answer = ask(&mut out, &mut answers, &cancel, &finding(&[3], &decisions));
    assert!(matches!(answer, Err(PromptError::Cancelled)));
}

struct Failing {
    cancel_first: Option<CancellationToken>,
}

impl Answers for Failing {
    fn line(&mut self, _: &CancellationToken) -> io::Result<Option<String>> {
        if let Some(cancel) = &self.cancel_first {
            cancel.cancel();
        }
        Err(io::Error::from_raw_os_error(5))
    }

    fn discard(&mut self) {}
}

#[test]
fn an_answer_that_could_not_be_read_is_no_unless_the_run_was_stopped() {
    let decisions = [];
    let mut out = Shared::default();
    let no = ask(
        &mut out,
        &mut Failing { cancel_first: None },
        &CancellationToken::new(),
        &finding(&[3], &decisions),
    );
    assert!(matches!(no, Ok(false)));

    let cancel = CancellationToken::new();
    let stopped = ask(
        &mut out,
        &mut Failing {
            cancel_first: Some(cancel.clone()),
        },
        &cancel,
        &finding(&[3], &decisions),
    );
    assert!(matches!(stopped, Err(PromptError::Cancelled)));
}

const EOF_KEY: &[u8] = b"\x04";

#[test]
fn real_fs_a_typed_line_is_read_whole_on_a_terminal() {
    let (master, slave) = open_pty().unwrap();
    write(&master, b"yes\n").unwrap();
    let line = read_line(slave.as_fd(), &CancellationToken::new(), readable).unwrap();
    assert_eq!(line.as_deref(), Some("yes\n"));
}

#[test]
fn real_fs_a_signal_that_interrupts_the_wait_is_a_wake_up_not_an_error() {
    let (_master, slave) = open_pty().unwrap();
    let cancel = CancellationToken::new();
    let mut waits = 0;
    let line = read_line(slave.as_fd(), &cancel, |_| {
        waits += 1;
        if waits == 1 {
            return Err(Errno::INTR);
        }
        cancel.cancel();
        Ok(false)
    })
    .unwrap();
    assert_eq!(line, None);
    assert_eq!(waits, 2);
}

#[test]
fn real_fs_a_part_of_a_line_does_not_keep_a_stop_waiting() {
    let (master, slave) = open_pty().unwrap();
    write(&master, b"abc").unwrap();
    write(&master, EOF_KEY).unwrap();
    let cancel = CancellationToken::new();
    let mut waits = 0;
    let line = read_line(slave.as_fd(), &cancel, |fd| {
        waits += 1;
        if waits > 2 {
            cancel.cancel();
            return Ok(false);
        }
        readable(fd)
    })
    .unwrap();
    assert_eq!(line, None);
    assert_eq!(waits, 3);
}

#[test]
fn real_fs_the_end_of_the_input_after_a_part_of_a_line_is_that_part_and_no() {
    let (master, slave) = open_pty().unwrap();
    write(&master, b"yes").unwrap();
    write(&master, EOF_KEY).unwrap();
    write(&master, EOF_KEY).unwrap();
    let line = read_line(slave.as_fd(), &CancellationToken::new(), readable).unwrap();
    assert_eq!(line.as_deref(), Some("yes"));
    assert!(!parse_answer("yes"));
}

#[test]
fn real_fs_the_end_of_the_input_with_nothing_typed_is_no_line() {
    let (master, slave) = open_pty().unwrap();
    write(&master, EOF_KEY).unwrap();
    let line = read_line(slave.as_fd(), &CancellationToken::new(), readable).unwrap();
    assert_eq!(line, None);
}
