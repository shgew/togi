use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use togi_defect::{DefectId, FindingView};
use togi_journal::{DefectDirection, Seq};
use togi_machine::{CancellationToken, CoreId, CoreSet};
use togi_session::PromptError;

use super::{Answers, ask, parse_answer};

#[test]
fn only_a_complete_line_saying_y_or_yes_is_a_yes() {
    let table = [
        ("Yes \n", true),
        ("Y\n", true),
        ("yEs\n", true),
        ("no\n", false),
        ("sure\n", false),
        ("\n", false),
        ("yes", false),
        ("", false),
    ];
    for (line, want) in table {
        assert_eq!(parse_answer(line), want, "{line:?}");
    }
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

#[test]
fn the_question_names_every_core_and_a_no_or_end_of_input_is_no() {
    for line in [Some("n\n"), None] {
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
}

#[test]
fn a_run_stopped_while_the_question_is_open_ends_the_wait_without_an_answer() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let out = Shared::default();
    let mut answers = Typed {
        out: out.clone(),
        line: None,
    };
    let decisions = [];
    let answer = ask(
        &mut out.clone(),
        &mut answers,
        &cancel,
        &finding(&[3], &decisions),
    );
    assert!(matches!(answer, Err(PromptError::Cancelled)));
}
