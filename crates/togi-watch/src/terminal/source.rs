//! The journal as the loop sees it: reloaded only when the file changed, folded incrementally, and with the log
//! list held still while the reader is scrolled back in it.

use std::fs::Metadata;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use togi_journal::{FileError, ReadError, Recording};

use crate::history::LogEntry;
use crate::model::{FoldError, Model};
use crate::screen::{Journal, Log, Screen, View};

/// Why the journal cannot be shown.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The journal's file cannot be examined.
    #[error("{path}: {source}", path = path.display())]
    Stat {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The journal cannot be read or does not decode.
    #[error(transparent)]
    Read(#[from] FileError),
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
    /// The folded journal, and how many of its events are in the model.
    Ready(Box<Model>, usize),
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
    /// With a run to wait for, the journal counts as starting until it holds an event after this sequence number:
    /// the last one before this run's session started.
    since: Option<u64>,
    identity: Option<Identity>,
    stat_error: Option<String>,
    content: Content,
    held: Option<HeldLog>,
}

fn is_not_found(error: &FileError) -> bool {
    matches!(&error.source, ReadError::Io(io) if io.kind() == io::ErrorKind::NotFound)
}

impl Source {
    pub(super) fn new(directory: &Path, since: Option<u64>) -> Self {
        Self {
            path: directory.join("events.jsonl"),
            since,
            identity: None,
            stat_error: None,
            content: Content::Missing,
            held: None,
        }
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
        let metadata = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(source) => return self.unavailable(source),
        };
        self.stat_error = None;
        let now = Identity::of(&metadata);
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
        if !matches!(&self.content, Content::Ready(model, _) if model.problem().is_none()) {
            self.held = None;
        }
        true
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
        let recording = match Recording::read(&self.path) {
            Ok(recording) => recording,
            Err(error) if is_not_found(&error) => {
                self.content = self.absent();
                return;
            }
            Err(error) => {
                self.content = Content::Problem(error.into());
                return;
            }
        };
        let events = recording.events();
        let waiting = self
            .since
            .is_some_and(|since| events.last().is_none_or(|last| last.seq.get() <= since));
        if waiting {
            self.content = Content::Starting;
            return;
        }
        if events.is_empty() {
            self.content = Content::Missing;
            return;
        }
        let (mut model, folded) = match std::mem::replace(&mut self.content, Content::Missing) {
            Content::Ready(model, folded)
                if appended && model.problem().is_none() && folded <= events.len() =>
            {
                (model, folded)
            }
            _ => (Box::new(Model::new()), 0),
        };
        for event in &events[folded..] {
            if model.fold(event).is_err() {
                break;
            }
        }
        self.content = Content::Ready(model, events.len());
    }

    /// Decides what the log view lists for this screen: the held list while it is scrolled back, else the live one.
    fn hold(&mut self, screen: &Screen) {
        let Content::Ready(model, _) = &self.content else {
            self.held = None;
            return;
        };
        if model.problem().is_some() || screen.view != View::Log || screen.scroll == usize::MAX {
            self.held = None;
            return;
        }
        let live = model.snapshot().log();
        if self.held.as_ref().is_none_or(|held| live.total < held.total) {
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
            Content::Ready(model, _) => {
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
            Content::Ready(model, _) => model.problem().cloned().map(SourceError::Fold),
            Content::Missing | Content::Starting => None,
        }
    }
}
