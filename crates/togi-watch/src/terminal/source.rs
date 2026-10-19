//! The journal as the loop sees it: reloaded only when the file changed, folded incrementally, and with the log
//! list held still while the reader is scrolled back in it.

use std::fs::Metadata;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use togi_journal::{
    Build, DirError, Event, FileError, FixStamp, Operation, Payload, ReadError, Recording, Refusal,
    SCHEMA, Verdict,
};

use crate::history::LogEntry;
use crate::model::{FoldError, Model};
use crate::screen::{Journal, Log, Screen, View};

/// Why the journal cannot be shown.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The journal's file cannot be examined.
    #[error("examine {path}", path = path.display())]
    Stat {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The journal cannot be read or does not decode.
    #[error(transparent)]
    Read(#[from] FileError),
    /// The journal was written by a build whose schema this one cannot show.
    #[error(transparent)]
    Incompatible(#[from] Refusal),
    /// The journal is another build's: its first line is not a journal header, and nothing after it is read.
    #[error(transparent)]
    Foreign(DirError),
    /// The journal decodes, but its events do not fold into a session.
    #[error(transparent)]
    Fold(#[from] FoldError),
}

/// What identifies a version of the journal file: which file it is, and its size and modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    size: u64,
    modified: Option<SystemTime>,
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }

    fn same_file(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

enum Content {
    Missing,
    Starting,
    Problem(SourceError),
    /// The folded journal, how many of its events are in the model, and the last of them.
    Ready(Box<Model>, usize, Box<Event>),
}

/// The journal view's list while it is scrolled back: the entries it held when the reader left the end. Events that
/// arrive later stay out of it, so the rows the reader is on do not move.
struct HeldLog {
    entries: Vec<LogEntry>,
    total: usize,
}

/// Reloads the journal when it changes and lends what the frames draw.
pub(super) struct Source {
    path: PathBuf,
    #[cfg(test)]
    memory: Option<MemoryJournal>,
    /// With a run to wait for, the journal counts as starting until it holds an event after this sequence number:
    /// the last one before this run's session started.
    since: Option<u64>,
    identity: Option<Identity>,
    stat_error: Option<String>,
    content: Content,
    held: Option<HeldLog>,
}

/// This build, as far as showing a journal goes: only its schema decides whether a journal can be shown.
fn shown_build() -> Build {
    Build {
        version: String::new(),
        rev: None,
        ruleset: 0,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: 0,
    }
}

fn is_not_found(error: &FileError) -> bool {
    matches!(&error.source, ReadError::Io(io) if io.kind() == io::ErrorKind::NotFound)
}

impl Source {
    pub(super) fn new(directory: &Path, since: Option<u64>) -> Self {
        Self {
            path: directory.join("events.jsonl"),
            since,
            #[cfg(test)]
            memory: None,
            identity: None,
            stat_error: None,
            content: Content::Missing,
            held: None,
        }
    }

    /// Loads a one-frame snapshot through an explicit filesystem, without consulting host metadata.
    pub(super) fn read_in(
        fs: &dyn togi_journal::fs::Fs,
        directory: &Path,
        since: Option<u64>,
    ) -> Self {
        let mut source = Self::new(directory, since);
        let recording = Recording::read_in_lenient(fs, &source.path);
        let saved = togi_journal::snapshot::read_in(fs, directory)
            .ok()
            .flatten();
        source.fold_recording(recording, false, saved);
        source
    }

    /// What the journal shows when there is no session in it.
    fn absent(&self) -> Content {
        if self.since.is_some() {
            Content::Starting
        } else {
            Content::Missing
        }
    }

    /// Reloads the journal if its file is a different one or changed size or modification time since the last
    /// load. Reports whether what is shown changed.
    pub(super) fn reload(&mut self) -> bool {
        let now = match self.current_identity() {
            Ok(identity) => identity,
            Err(source) => return self.unavailable(source),
        };
        self.stat_error = None;
        let mut appended = false;
        if let Some(before) = self.identity {
            let same = before.same_file(now);
            if same && now.size == before.size && now.modified == before.modified {
                return false;
            }
            if !same || now.size < before.size {
                self.held = None;
            }
            appended = same && now.size > before.size;
        }
        self.identity = Some(now);
        self.load(appended);
        if !matches!(&self.content, Content::Ready(model, ..) if model.problem().is_none()) {
            self.held = None;
        }
        true
    }

    fn current_identity(&self) -> io::Result<Identity> {
        #[cfg(test)]
        if let Some(memory) = &self.memory {
            return memory.identity();
        }
        std::fs::metadata(&self.path).map(|metadata| Identity::of(&metadata))
    }

    fn recording(&self) -> Result<Recording, FileError> {
        #[cfg(test)]
        if let Some(memory) = &self.memory {
            let state = memory.0.borrow();
            return Recording::read_in_lenient(&state.fs, &self.path);
        }
        Recording::read_lenient(&self.path)
    }

    fn unavailable(&mut self, source: io::Error) -> bool {
        let text = source.to_string();
        if self.stat_error.as_deref() == Some(&text) {
            return false;
        }
        self.held = None;
        self.identity = None;
        self.stat_error = Some(text);
        self.content = if source.kind() == io::ErrorKind::NotFound {
            self.absent()
        } else {
            Content::Problem(SourceError::Stat {
                path: self.path.clone(),
                source,
            })
        };
        true
    }

    fn load(&mut self, appended: bool) {
        let recording = self.recording();
        let saved = if appended
            && matches!(&self.content, Content::Ready(model, ..) if model.problem().is_none())
        {
            None
        } else {
            self.saved_snapshot()
        };
        self.fold_recording(recording, appended, saved);
    }

    fn saved_snapshot(
        &self,
    ) -> Option<(togi_journal::snapshot::Prefix, crate::model::PublishedView)> {
        let directory = self.path.parent()?;
        #[cfg(test)]
        if let Some(memory) = &self.memory {
            return togi_journal::snapshot::read_in(&memory.0.borrow().fs, directory)
                .ok()
                .flatten();
        }
        togi_journal::snapshot::read(directory).ok().flatten()
    }

    fn fold_recording(
        &mut self,
        recording: Result<Recording, FileError>,
        appended: bool,
        saved: Option<(togi_journal::snapshot::Prefix, crate::model::PublishedView)>,
    ) {
        let recording = match recording {
            Ok(recording) => recording,
            Err(error) if is_not_found(&error) => {
                self.content = self.absent();
                return;
            }
            Err(FileError {
                path,
                source: ReadError::Foreign,
            }) => {
                self.content =
                    Content::Problem(SourceError::Foreign(DirError::ForeignJournal { path }));
                return;
            }
            Err(error) => {
                self.content = Content::Problem(error.into());
                return;
            }
        };
        let events = recording.events();
        if let Some(Event {
            payload: Payload::SessionStart(start),
            ..
        }) = events.first()
        {
            if let Verdict::Refuse(refusal) =
                shown_build().verdict(&start.build, Operation::Inspect)
            {
                self.content = Content::Problem(refusal.into());
                return;
            }
        }
        let waiting = self
            .since
            .is_some_and(|since| events.last().is_none_or(|last| last.seq.get() <= since));
        if waiting {
            self.content = Content::Starting;
            return;
        }
        let Some(last) = events.last().cloned() else {
            self.content = Content::Missing;
            return;
        };
        let (mut model, folded) = match std::mem::replace(&mut self.content, Content::Missing) {
            Content::Ready(model, folded, before)
                if appended
                    && model.problem().is_none()
                    && folded
                        .checked_sub(1)
                        .and_then(|index| events.get(index))
                        .is_some_and(|event| *event == *before) =>
            {
                (model, folded)
            }
            Content::Ready(..) if appended => {
                self.held = None;
                let (model, folded) = Model::from_snapshot(events, saved);
                (Box::new(model), folded)
            }
            Content::Missing | Content::Starting | Content::Problem(_) | Content::Ready(..) => {
                let (model, folded) = Model::from_snapshot(events, saved);
                (Box::new(model), folded)
            }
        };
        let batch = &events[folded..];
        let checked = match model.fold_batch(batch) {
            Err(_) if folded != 0 => {
                model = Box::new(crate::model::project(events));
                events
            }
            Err(error) => &batch[..=error.index],
            Ok(()) => batch,
        };
        for event in checked {
            if let Payload::SessionStart(start) = &event.payload {
                if let Verdict::Refuse(refusal) =
                    shown_build().verdict(&start.build, Operation::Inspect)
                {
                    self.content = Content::Problem(refusal.into());
                    return;
                }
            }
        }
        let skipped = recording.skipped();
        model.set_skipped((!skipped.is_empty()).then(|| skipped.to_string()));
        self.content = Content::Ready(model, events.len(), Box::new(last));
    }

    /// Decides what the log view lists for this screen: the held list while it is scrolled back, else the live one.
    fn hold(&mut self, screen: &Screen) {
        let Content::Ready(model, ..) = &self.content else {
            self.held = None;
            return;
        };
        if model.problem().is_some() || screen.view != View::Log || screen.scroll == usize::MAX {
            self.held = None;
            return;
        }
        let live = model.snapshot().log();
        if self
            .held
            .as_ref()
            .is_none_or(|held| live.total < held.total)
        {
            self.held = Some(HeldLog {
                entries: live.entries.to_vec(),
                total: live.total,
            });
        }
    }

    /// The journal for a frame of `screen`.
    pub(super) fn journal(&mut self, screen: &Screen) -> Journal<'_> {
        self.hold(screen);
        match &self.content {
            Content::Missing => Journal::Missing,
            Content::Starting => Journal::Starting,
            Content::Problem(error) => Journal::Problem(error),
            Content::Ready(model, ..) => {
                if let Some(error) = model.problem() {
                    return Journal::Problem(error);
                }
                let snapshot = model.snapshot();
                let live = snapshot.log();
                let log = self.held.as_ref().map_or(live, |held| Log {
                    entries: &held.entries,
                    total: held.total,
                    arrived: live.total.saturating_sub(held.total),
                });
                Journal::Ready { snapshot, log }
            }
        }
    }
}

impl Source {
    /// Consumes the source and returns why the journal cannot be shown, if it cannot.
    pub(super) fn into_problem(self) -> Option<SourceError> {
        match self.content {
            Content::Problem(error) => Some(error),
            Content::Ready(model, ..) => model.problem().cloned().map(SourceError::Fold),
            Content::Missing | Content::Starting => None,
        }
    }

    /// Takes the error that ends the dashboard instead of being shown, which is another build's journal. Every other
    /// problem stays to be shown and retried when the journal changes.
    pub(super) fn refusal(&mut self) -> Option<SourceError> {
        match &self.content {
            Content::Problem(SourceError::Foreign(_)) => {
                match std::mem::replace(&mut self.content, Content::Missing) {
                    Content::Problem(error) => Some(error),
                    Content::Missing | Content::Starting | Content::Ready(..) => None,
                }
            }
            Content::Missing | Content::Starting | Content::Problem(_) | Content::Ready(..) => None,
        }
    }
}

impl super::Feed for Source {
    fn journal(&mut self, screen: &Screen) -> Journal<'_> {
        Self::journal(self, screen)
    }

    fn reload(&mut self) -> bool {
        Self::reload(self)
    }

    fn refusal(&mut self) -> Option<SourceError> {
        Self::refusal(self)
    }
}

#[cfg(test)]
impl Source {
    pub(super) fn memory(journal: &MemoryJournal, since: Option<u64>) -> Self {
        let mut source = Self::new(Path::new("memory"), since);
        source.memory = Some(journal.clone());
        source
    }

    /// Where the model is, which changes only when a fresh one replaces it.
    fn model_address(&self) -> Option<usize> {
        match &self.content {
            Content::Ready(model, ..) => Some(std::ptr::from_ref::<Model>(model) as usize),
            Content::Missing | Content::Starting | Content::Problem(_) => None,
        }
    }
}

/// A journal with controlled identities and bytes, without paths resolved on the host.
#[cfg(test)]
#[derive(Clone)]
pub(super) struct MemoryJournal(std::rc::Rc<std::cell::RefCell<MemoryState>>);

#[cfg(test)]
struct MemoryState {
    fs: togi_journal::MemFs,
    bytes: Option<Vec<u8>>,
    inode: u64,
    modified: SystemTime,
}

#[cfg(test)]
impl MemoryJournal {
    pub(super) fn new() -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(MemoryState {
            fs: togi_journal::MemFs::new(),
            bytes: None,
            inode: 1,
            modified: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2000),
        })))
    }

    fn identity(&self) -> io::Result<Identity> {
        let state = self.0.borrow();
        let bytes = state
            .bytes
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "journal is missing"))?;
        Ok(Identity {
            device: 1,
            inode: state.inode,
            size: bytes.len() as u64,
            modified: Some(state.modified),
        })
    }

    pub(super) fn write(&self, bytes: &[u8]) {
        let mut state = self.0.borrow_mut();
        state.fs.put("memory/events.jsonl", bytes.to_vec());
        state.bytes = Some(bytes.to_vec());
    }

    pub(super) fn append(&self, bytes: &[u8]) {
        let mut contents = self.bytes();
        contents.extend_from_slice(bytes);
        self.write(&contents);
    }

    pub(super) fn replace(&self, bytes: &[u8]) {
        self.0.borrow_mut().inode += 1;
        self.write(bytes);
    }

    pub(super) fn remove(&self) {
        self.0.borrow_mut().bytes = None;
    }

    /// Bytes, modification time, entry count and mutation count for read-only assertions.
    pub(super) fn snapshot(&self) -> (Option<Vec<u8>>, SystemTime, usize, u64) {
        let state = self.0.borrow();
        (
            state.bytes.clone(),
            state.modified,
            state.fs.entries().len(),
            state.fs.mutations(),
        )
    }

    fn bytes(&self) -> Vec<u8> {
        self.0.borrow().bytes.clone().expect("journal exists")
    }

    fn touch(&self, seconds: u64) {
        self.0.borrow_mut().modified =
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::{Content, MemoryJournal, Source, SourceError};
    use crate::history::LOG_LIMIT;
    use crate::screen::{Colours, Journal, Quit, Screen, View};
    use crate::terminal::testing::{
        go_journal, lines, real_fs_write_journal, session_start, session_with_warnings,
        undecodable_journal, warning,
    };

    /// What a frame would be drawn from.
    #[derive(Debug, PartialEq, Eq)]
    enum Shown {
        Missing,
        Starting,
        Problem(String),
        Ready {
            messages: Vec<String>,
            total: usize,
            arrived: usize,
        },
    }

    fn screen(view: View, scroll: usize) -> Screen {
        Screen {
            view,
            keys: true,
            scroll,
            width: 120,
            height: 33,
            one_frame: false,
            colours: Colours::None,
            quit: Quit::Keys,
        }
    }

    fn main_screen() -> Screen {
        screen(View::Main, 0)
    }

    const FOLLOWING: usize = usize::MAX;

    fn show(source: &mut Source, screen: &Screen) -> Shown {
        match source.journal(screen) {
            Journal::Missing => Shown::Missing,
            Journal::Starting => Shown::Starting,
            Journal::Problem(error) => Shown::Problem(togi_render::error_chain(error)),
            Journal::Ready { log, .. } => Shown::Ready {
                messages: log.entries.iter().map(|entry| entry.msg.clone()).collect(),
                total: log.total,
                arrived: log.arrived,
            },
        }
    }

    fn ready(shown: &Shown) -> (&[String], usize, usize) {
        match shown {
            Shown::Ready {
                messages,
                total,
                arrived,
            } => (messages, *total, *arrived),
            other @ (Shown::Missing | Shown::Starting | Shown::Problem(_)) => {
                panic!("not a journal: {other:?}")
            }
        }
    }

    fn contains(source: &mut Source, text: &str) -> bool {
        let shown = show(source, &main_screen());
        ready(&shown).0.iter().any(|message| message == text)
    }

    fn real_fs_replace(dir: &Path, bytes: &[u8]) {
        let temporary = dir.join("replacement");
        fs::write(&temporary, bytes).unwrap();
        let file = fs::File::options().write(true).open(&temporary).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(2000))
            .unwrap();
        fs::rename(&temporary, dir.join("events.jsonl")).unwrap();
    }

    fn journal_of(message: &str) -> Vec<crate::terminal::testing::TestEvent> {
        vec![session_start(1), warning(2, message)]
    }

    #[test]
    fn an_event_of_an_unknown_kind_is_skipped_and_reported_in_the_snapshot() {
        let dir = MemoryJournal::new();
        dir.write(&lines(&journal_of("original")));
        dir.append(
            b"{\"seq\":3,\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"boot-1\",\"kind\":\"future.thing\",\"msg\":\"x\",\"data\":{}}\n",
        );
        dir.append(&lines(&[warning(4, "after")]));
        let mut source = Source::memory(&dir, None);
        assert!(source.reload());
        let Journal::Ready { snapshot, log } = source.journal(&main_screen()) else {
            panic!("the journal is not shown");
        };
        assert_eq!(
            snapshot.skipped,
            Some("skipped 1 event of a kind this build does not know (future.thing at line 3)")
        );
        assert!(log.entries.iter().any(|entry| entry.msg == "after"));

        dir.append(&lines(&[warning(5, "later")]));
        assert!(source.reload());
        let Journal::Ready { snapshot, .. } = source.journal(&main_screen()) else {
            panic!("the journal is not shown");
        };
        assert!(snapshot.skipped.is_some(), "the report outlives a reload");
    }

    #[test]
    fn reload_follows_a_replaced_grown_rewritten_removed_and_restored_journal() {
        let dir = MemoryJournal::new();
        let mut source = Source::memory(&dir, None);
        assert!(source.reload());
        assert_eq!(show(&mut source, &main_screen()), Shown::Missing);

        dir.write(&lines(&journal_of("original")));
        assert!(source.reload());
        assert!(contains(&mut source, "original"));

        dir.replace(&lines(&journal_of("replaced")));
        assert!(
            source.reload(),
            "a replacement of the same size and time is a new file"
        );
        assert!(contains(&mut source, "replaced") && !contains(&mut source, "original"));

        dir.append(&lines(&[warning(3, "appended")]));
        assert!(source.reload(), "growth is a change");
        assert!(contains(&mut source, "appended") && contains(&mut source, "replaced"));

        let grown = String::from_utf8(dir.bytes()).unwrap();
        dir.write(grown.replace("appended", "modified").as_bytes());
        dir.touch(2001);
        assert!(source.reload(), "a new modification time is a change");
        assert!(contains(&mut source, "modified") && !contains(&mut source, "appended"));

        dir.remove();
        assert!(source.reload());
        assert_eq!(show(&mut source, &main_screen()), Shown::Missing);

        dir.write(&lines(&journal_of("restored")));
        assert!(source.reload());
        assert!(contains(&mut source, "restored"));
    }

    #[test]
    fn real_fs_a_replacement_with_the_same_size_and_time_is_a_new_file() {
        let dir = TempDir::new().unwrap();
        real_fs_write_journal(dir.path(), &journal_of("original"));
        let mut source = Source::new(dir.path(), None);
        assert!(source.reload());
        let before = source.identity.unwrap();
        real_fs_replace(dir.path(), &lines(&journal_of("replaced")));
        let after = source.current_identity().unwrap();
        assert_eq!((before.size, before.modified), (after.size, after.modified));
        assert!(!before.same_file(after), "rename did not replace the inode");
        assert!(source.reload());
        assert!(contains(&mut source, "replaced") && !contains(&mut source, "original"));
    }

    #[test]
    fn an_unchanged_journal_and_an_unchanged_absence_are_not_reloaded() {
        let dir = MemoryJournal::new();
        let mut source = Source::memory(&dir, None);
        assert!(source.reload());
        assert!(
            !source.reload(),
            "an unchanged missing journal was reloaded"
        );
        dir.write(&lines(&journal_of("original")));
        assert!(source.reload());
        assert!(!source.reload(), "an unchanged journal was reloaded");
    }

    #[test]
    fn an_append_folds_only_the_new_events_and_a_replacement_builds_a_fresh_model() {
        let dir = MemoryJournal::new();
        dir.write(&lines(&session_with_warnings(5)));
        let mut source = Source::memory(&dir, None);
        source.reload();
        let first = source.model_address().unwrap();
        dir.append(&lines(&[warning(7, "w7")]));
        source.reload();
        dir.append(&lines(&[warning(8, "w8")]));
        source.reload();
        assert_eq!(
            source.model_address(),
            Some(first),
            "an append rebuilt the model"
        );
        assert!(
            matches!(&source.content, Content::Ready(model, 8, _) if model.problem().is_none())
        );
        dir.replace(&lines(&session_with_warnings(2)));
        source.reload();
        let replaced = source.model_address().unwrap();
        assert_ne!(replaced, first);
        assert!(matches!(&source.content, Content::Ready(_, 3, _)));
        dir.write(&lines(&session_with_warnings(1)));
        source.reload();
        assert_ne!(
            source.model_address(),
            Some(replaced),
            "a truncation kept the model"
        );
        assert!(matches!(&source.content, Content::Ready(_, 2, _)));
    }

    #[test]
    fn a_journal_rewritten_in_place_to_a_longer_one_builds_a_fresh_model_and_drops_the_held_list() {
        let dir = MemoryJournal::new();
        let (mut source, back) = scrolled_back(&dir, 40);
        let first = source.model_address().unwrap();
        let before = dir.identity().unwrap();
        let mut rewritten = vec![session_start(1)];
        rewritten.extend((2..42).map(|seq| warning(seq, &format!("rewritten {seq}"))));
        dir.write(&lines(&rewritten));
        let after = dir.identity().unwrap();
        assert_eq!(
            (after.inode, after.modified),
            (before.inode, before.modified)
        );
        assert!(after.size > before.size);
        assert!(source.reload());
        assert_ne!(
            source.model_address(),
            Some(first),
            "the rewritten journal was folded onto the old model"
        );
        let shown = show(&mut source, &back);
        let (rows, total, arrived) = ready(&shown);
        assert!(rows.iter().any(|row| row == "rewritten 41"), "{rows:?}");
        assert!(!rows.iter().any(|row| row == "w41"), "{rows:?}");
        assert_eq!((total, arrived), (rows.len(), 0));
    }

    #[test]
    fn a_journal_of_a_newer_schema_is_a_problem_not_a_session() {
        let dir = MemoryJournal::new();
        let text = String::from_utf8(lines(&journal_of("newer"))).unwrap();
        let newer = text.replace("\"schema\":1", "\"schema\":2");
        assert_ne!(text, newer);
        dir.write(newer.as_bytes());
        let mut source = Source::memory(&dir, None);
        source.reload();
        let Shown::Problem(text) = show(&mut source, &main_screen()) else {
            panic!("a newer journal was shown");
        };
        assert_eq!(text, "the journal's schema is 2; this build's is 1");
    }

    #[test]
    fn a_journal_that_does_not_decode_is_a_problem_until_it_does() {
        let dir = MemoryJournal::new();
        let mut source = Source::memory(&dir, None);
        source.reload();
        dir.write(&undecodable_journal());
        assert!(source.reload());
        let Shown::Problem(text) = show(&mut source, &main_screen()) else {
            panic!("an undecodable journal was shown");
        };
        assert!(text.contains("line 2"), "{text}");
        assert!(!source.reload(), "an unchanged problem was reloaded");
        dir.write(&lines(&journal_of("live journal recovered")));
        assert!(source.reload());
        assert!(contains(&mut source, "live journal recovered"));
    }

    #[test]
    fn a_journal_the_model_cannot_fold_is_a_problem_and_the_error_of_a_one_frame_print() {
        let dir = MemoryJournal::new();
        dir.write(&lines(&[session_start(1), session_start(2)]));
        let mut source = Source::memory(&dir, None);
        source.reload();
        assert_eq!(
            show(&mut source, &main_screen()),
            Shown::Problem("event #2 is a second session.start".into())
        );
        let problem = source.into_problem().unwrap();
        assert_eq!(problem.to_string(), "event #2 is a second session.start");
    }

    #[test]
    fn real_fs_missing_and_unreadable_are_distinct() {
        let dir = TempDir::new().unwrap();
        let mut source = Source::new(dir.path(), None);
        source.reload();
        assert_eq!(show(&mut source, &main_screen()), Shown::Missing);
        assert!(source.into_problem().is_none());

        fs::create_dir(dir.path().join("events.jsonl")).unwrap();
        let mut source = Source::new(dir.path(), None);
        source.reload();
        assert!(
            matches!(show(&mut source, &main_screen()), Shown::Problem(_)),
            "a journal that cannot be read was shown as missing"
        );
        assert!(source.into_problem().is_some());

        if !rustix::process::geteuid().is_root() {
            let hidden = TempDir::new().unwrap();
            let inner = hidden.path().join("state");
            fs::create_dir(&inner).unwrap();
            real_fs_write_journal(&inner, &journal_of("hidden"));
            fs::set_permissions(&inner, std::os::unix::fs::PermissionsExt::from_mode(0o000))
                .unwrap();
            let mut source = Source::new(&inner, None);
            source.reload();
            let shown = show(&mut source, &main_screen());
            fs::set_permissions(&inner, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            assert!(matches!(shown, Shown::Problem(_)), "{shown:?}");
        }
    }

    #[test]
    fn an_empty_journal_has_no_session_yet() {
        let dir = MemoryJournal::new();
        dir.write(b"");
        let mut source = Source::memory(&dir, None);
        source.reload();
        assert_eq!(show(&mut source, &main_screen()), Shown::Missing);
    }

    #[test]
    fn a_run_in_progress_starts_with_the_run_not_the_journal_it_found() {
        let dir = MemoryJournal::new();
        let mut source = Source::memory(&dir, Some(2));
        source.reload();
        assert_eq!(show(&mut source, &main_screen()), Shown::Starting);
        dir.write(&lines(&journal_of("from an earlier run")));
        assert!(source.reload());
        assert_eq!(
            show(&mut source, &main_screen()),
            Shown::Starting,
            "an earlier run's journal was shown"
        );
        dir.append(&lines(&[warning(3, "this run")]));
        assert!(source.reload());
        assert!(contains(&mut source, "this run"));
        assert!(contains(&mut source, "from an earlier run"));
    }

    #[test]
    fn a_run_in_progress_still_reports_a_journal_it_cannot_read() {
        let dir = MemoryJournal::new();
        dir.write(&undecodable_journal());
        let mut source = Source::memory(&dir, Some(0));
        source.reload();
        assert!(matches!(
            show(&mut source, &main_screen()),
            Shown::Problem(_)
        ));
    }

    #[test]
    fn another_builds_journal_is_a_refusal_the_source_hands_over_once() {
        for since in [None, Some(0)] {
            let dir = MemoryJournal::new();
            dir.write(&go_journal());
            let mut source = Source::memory(&dir, since);
            assert!(source.reload());
            let Shown::Problem(text) = show(&mut source, &main_screen()) else {
                panic!("another build's journal was shown");
            };
            assert!(text.contains("events.jsonl"), "{text}");
            assert!(text.contains("docs/howto.md#go-to-rust-cutover"), "{text}");
            assert!(!text.contains("line 2"), "no later line was read: {text}");
            let refusal = source.refusal().unwrap();
            assert!(matches!(
                refusal,
                SourceError::Foreign(togi_journal::DirError::ForeignJournal { .. })
            ));
            assert_eq!(refusal.to_string(), text);
            assert!(source.refusal().is_none());
        }
    }

    #[test]
    fn every_other_problem_stays_to_be_shown_and_is_not_a_refusal() {
        let dir = MemoryJournal::new();
        dir.write(&undecodable_journal());
        let mut source = Source::memory(&dir, None);
        source.reload();
        assert!(matches!(
            show(&mut source, &main_screen()),
            Shown::Problem(_)
        ));
        assert!(source.refusal().is_none());
        assert!(source.into_problem().is_some());
    }

    #[test]
    fn a_journal_that_turns_into_another_builds_after_a_missing_one_is_a_refusal() {
        let dir = MemoryJournal::new();
        let mut source = Source::memory(&dir, None);
        source.reload();
        assert!(source.refusal().is_none());
        dir.write(&go_journal());
        assert!(source.reload());
        assert!(source.refusal().is_some());
    }

    fn scrolled_back(dir: &MemoryJournal, warnings: u64) -> (Source, Screen) {
        scrolled_back_over(dir, &session_with_warnings(warnings))
    }

    fn scrolled_back_over(
        dir: &MemoryJournal,
        events: &[crate::terminal::testing::TestEvent],
    ) -> (Source, Screen) {
        dir.write(&lines(events));
        let mut source = Source::memory(dir, None);
        assert!(source.reload());
        show(&mut source, &screen(View::Log, FOLLOWING));
        let back = screen(View::Log, 0);
        show(&mut source, &back);
        (source, back)
    }

    /// The fake journal's inode, size and modification time.
    fn file_identity(dir: &MemoryJournal) -> (u64, u64, std::time::SystemTime) {
        let identity = dir.identity().unwrap();
        (identity.inode, identity.size, identity.modified.unwrap())
    }

    #[test]
    fn a_scrolled_back_log_holds_its_list_and_counts_arrivals() {
        let dir = MemoryJournal::new();
        let full = u64::try_from(LOG_LIMIT).unwrap() + 50;
        dir.write(&lines(&session_with_warnings(full)));
        let mut source = Source::memory(&dir, None);
        source.reload();
        let following = screen(View::Log, FOLLOWING);
        let shown = show(&mut source, &following);
        assert_eq!(ready(&shown).2, 0, "a view at the end counts arrivals");

        let back = screen(View::Log, 0);
        let held = show(&mut source, &back);
        let (held_rows, held_total, _) = ready(&held);
        let held_rows = held_rows.to_vec();
        assert_eq!(held_rows.len(), LOG_LIMIT);

        dir.append(&lines(&[warning(full + 2, "arrival")]));
        source.reload();
        let one = show(&mut source, &back);
        assert_eq!(
            (ready(&one).0, ready(&one).1, ready(&one).2),
            (&held_rows[..], held_total, 1)
        );

        for seq in full + 3..full + 10 {
            dir.append(&lines(&[warning(seq, "arrival")]));
        }
        source.reload();
        let many = show(&mut source, &back);
        assert_eq!(
            (ready(&many).0, ready(&many).1, ready(&many).2),
            (&held_rows[..], held_total, 8)
        );

        let end = show(&mut source, &following);
        let (rows, total, arrived) = ready(&end);
        assert_eq!(rows.last().map(String::as_str), Some("arrival"));
        assert_eq!(
            (total, arrived),
            (held_total + 8, 0),
            "a view back at the end counts arrivals"
        );

        let again = show(&mut source, &back);
        assert_eq!(ready(&again).2, 0, "a new hold starts with arrivals");
        assert_eq!(
            ready(&again).1,
            held_total + 8,
            "scrolling back again holds the list as it is now"
        );
    }

    #[test]
    fn closing_a_scrolled_back_log_lets_go_of_its_list() {
        let dir = MemoryJournal::new();
        let (mut source, back) = scrolled_back(&dir, 450);
        let _ = back;
        dir.append(&lines(&[warning(452, "latest")]));
        source.reload();
        show(&mut source, &main_screen());
        let reopened = show(&mut source, &screen(View::Log, FOLLOWING));
        let (rows, _, arrived) = ready(&reopened);
        assert_eq!(rows.last().map(String::as_str), Some("latest"));
        assert_eq!(arrived, 0);
    }

    #[test]
    fn a_scrolled_back_log_drops_its_list_when_the_journal_goes_away_or_breaks() {
        for unavailable in ["missing", "malformed", "empty"] {
            for redraw in [false, true] {
                let dir = MemoryJournal::new();
                let (mut source, back) = scrolled_back(&dir, 40);
                dir.append(&lines(&[warning(42, "old arrival")]));
                source.reload();
                assert_eq!(ready(&show(&mut source, &back)).2, 1);
                match unavailable {
                    "missing" => dir.remove(),
                    "malformed" => {
                        let mut text = dir.bytes();
                        text.extend_from_slice(b"not json\n");
                        dir.write(&text);
                    }
                    _ => dir.write(b""),
                }
                assert!(source.reload(), "{unavailable}");
                if redraw {
                    let shown = show(&mut source, &back);
                    assert_eq!(
                        matches!(shown, Shown::Problem(_)),
                        unavailable == "malformed",
                        "{unavailable}: {shown:?}"
                    );
                    assert!(
                        !matches!(shown, Shown::Ready { .. }),
                        "unavailable journal kept old rows"
                    );
                }
                assert!(
                    !source.reload(),
                    "unchanged unavailable journal was reloaded again"
                );
                let before = (unavailable != "missing").then(|| file_identity(&dir));
                let mut recovered = session_with_warnings(40);
                recovered.push(warning(42, &format!("recovered {}", "padding ".repeat(4))));
                dir.write(&lines(&recovered));
                if let Some((inode, size, _)) = before {
                    let (now_inode, now_size, _) = file_identity(&dir);
                    assert_eq!(now_inode, inode, "{unavailable}");
                    assert!(now_size > size, "{unavailable}: the recovery did not grow");
                }
                assert!(source.reload());
                let shown = show(&mut source, &back);
                let (rows, _, arrived) = ready(&shown);
                assert!(
                    rows.iter().any(|row| row.starts_with("recovered ")),
                    "{unavailable}/{redraw}: {rows:?}"
                );
                assert!(
                    !rows.iter().any(|row| row == "old arrival"),
                    "{unavailable}"
                );
                assert_eq!(
                    arrived, 0,
                    "{unavailable}/{redraw}: the recovered journal inherited arrivals"
                );
            }
        }
    }

    #[test]
    fn a_scrolled_back_log_drops_its_list_when_the_journal_is_replaced_or_truncated() {
        for operation in ["replace", "shrink"] {
            for warnings in [41_u64, 42] {
                let dir = MemoryJournal::new();
                let mut padded = vec![session_start(1)];
                padded.extend(
                    (2..42).map(|seq| warning(seq, &format!("w{seq} {}", "padding ".repeat(40)))),
                );
                let (mut source, back) = scrolled_back_over(&dir, &padded);
                dir.append(&lines(&[warning(42, "old arrival")]));
                source.reload();
                assert_eq!(ready(&show(&mut source, &back)).2, 1);
                let mut changed = session_with_warnings(warnings);
                changed.push(warning(warnings + 2, "replaced"));
                if operation == "replace" {
                    dir.replace(&lines(&changed));
                } else {
                    let (inode, size, modified) = file_identity(&dir);
                    dir.write(&lines(&changed));
                    let (now_inode, now_size, now_modified) = file_identity(&dir);
                    assert_eq!((now_inode, now_modified), (inode, modified));
                    assert!(now_size < size, "the rewrite did not shrink the journal");
                }
                assert!(source.reload(), "{operation}/{warnings}");
                let shown = show(&mut source, &back);
                let (rows, total, arrived) = ready(&shown);
                assert_eq!(
                    total,
                    usize::try_from(warnings).unwrap() + 2,
                    "{operation}/{warnings}: {rows:?}"
                );
                assert!(
                    !rows.iter().any(|row| row == "old arrival"),
                    "{operation}/{warnings}"
                );
                assert_eq!(
                    arrived, 0,
                    "{operation}/{warnings}: the changed journal inherited arrivals"
                );
                assert_eq!(total, rows.len(), "the list is the changed journal's own");
            }
        }
    }

    #[test]
    fn a_second_session_start_is_a_fold_problem_that_drops_the_held_list() {
        let dir = MemoryJournal::new();
        let (mut source, back) = scrolled_back(&dir, 40);
        dir.append(&lines(&[session_start(42)]));
        assert!(source.reload());
        assert!(matches!(show(&mut source, &back), Shown::Problem(_)));
        assert!(source.held.is_none());
    }

    #[test]
    fn a_scrolled_back_log_holds_through_appends() {
        let dir = MemoryJournal::new();
        let (mut source, back) = scrolled_back(&dir, 40);
        let before = show(&mut source, &back);
        let (rows, total, _) = ready(&before);
        let rows = rows.to_vec();
        for (count, event) in [
            warning(42, "ordinary arrival"),
            warning(43, "another arrival"),
            warning(44, "latest arrival"),
        ]
        .into_iter()
        .enumerate()
        {
            dir.append(&lines(&[event]));
            assert!(source.reload());
            let shown = show(&mut source, &back);
            let (held, held_total, arrived) = ready(&shown);
            assert_eq!(held, &rows[..], "arrival {} moved the held rows", count + 1);
            assert_eq!((held_total, arrived), (total, count + 1));
        }
        let end = show(&mut source, &screen(View::Log, FOLLOWING));
        assert_eq!(
            ready(&end).0.last().map(String::as_str),
            Some("latest arrival")
        );
        assert_eq!(ready(&end).2, 0);
    }
}
