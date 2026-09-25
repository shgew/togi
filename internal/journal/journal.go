package journal

import (
	"bytes"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"syscall"
	"time"
)

const (
	eventsFile = "events.jsonl"
	lockFile   = "lock"
	archiveDir = "archive"
	trialsDir  = "trials"
)

type Options struct {
	Boot     string
	Now      func() time.Time
	Sync     bool
	Log      io.Writer
	Renderer Renderer
}

var ErrLocked = errors.New("another shycler process holds the journal lock")

type Journal struct {
	dir    string
	opts   Options
	lock   *os.File
	f      *os.File
	events []Event
}

type Folder interface {
	Fold(Event)
}

func Open(dir string, opts Options) (*Journal, error) {
	j, err := open(dir, opts)
	if err != nil {
		return nil, fmt.Errorf("open journal %s: %w", dir, err)
	}
	return j, nil
}

func open(dir string, opts Options) (*Journal, error) {
	if opts.Now == nil {
		opts.Now = time.Now
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, err
	}
	lock, err := os.OpenFile(filepath.Join(dir, lockFile), os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return nil, err
	}
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		lock.Close()
		if errors.Is(err, syscall.EWOULDBLOCK) {
			return nil, ErrLocked
		}
		return nil, fmt.Errorf("lock: %w", err)
	}
	path := filepath.Join(dir, eventsFile)
	data, err := os.ReadFile(path)
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		lock.Close()
		return nil, err
	}
	events, end, err := parse(data)
	if err != nil {
		lock.Close()
		return nil, err
	}
	if n := len(events); n > 0 && end == len(data) {
		if a, ok := events[n-1].Data.(*SessionArchived); ok {
			if err := finishArchive(dir, a.Path, opts.Sync); err != nil {
				lock.Close()
				return nil, fmt.Errorf("finish archive: %w", err)
			}
			events, data, end = nil, nil, 0
		}
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		lock.Close()
		return nil, err
	}
	if opts.Sync {
		for _, d := range []string{dir, filepath.Dir(dir)} {
			if err := syncDir(d); err != nil {
				f.Close()
				lock.Close()
				return nil, fmt.Errorf("sync directory %s: %w", d, err)
			}
		}
	}
	j := &Journal{dir: dir, opts: opts, lock: lock, f: f, events: events}
	if end == len(data) {
		return j, nil
	}
	if err := f.Truncate(int64(end)); err != nil {
		j.Close()
		return nil, fmt.Errorf("truncate torn tail: %w", err)
	}
	if len(events) > 0 {
		torn := &JournalTorn{Offset: int64(end), BytesHex: hex.EncodeToString(data[end:])}
		if _, err := j.Append(torn); err != nil {
			j.Close()
			return nil, err
		}
	} else if opts.Sync {
		if err := f.Sync(); err != nil {
			j.Close()
			return nil, err
		}
	}
	return j, nil
}

func parse(data []byte) (events []Event, end int, err error) {
	for {
		i := bytes.IndexByte(data[end:], '\n')
		if i < 0 {
			return events, end, nil
		}
		line := data[end : end+i]
		n := len(events) + 1
		e, err := decode(line)
		if err != nil {
			return nil, 0, fmt.Errorf("journal line %d: %w", n, err)
		}
		if e.Seq != n {
			return nil, 0, fmt.Errorf("journal line %d: seq %d, want %d", n, e.Seq, n)
		}
		if n == 1 {
			start, ok := e.Data.(*SessionStart)
			if !ok {
				return nil, 0, fmt.Errorf("journal line 1: first event is %s, want %s", e.Kind, KindSessionStart)
			}
			if start.Schema != Schema {
				return nil, 0, fmt.Errorf("journal line 1: schema %d, want %d", start.Schema, Schema)
			}
		}
		events = append(events, e)
		end += i + 1
	}
}

func Read(dir string) (events []Event, torn []byte, err error) {
	return ReadFile(filepath.Join(dir, eventsFile))
}

func ReadFile(path string) (events []Event, torn []byte, err error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, nil, fmt.Errorf("read journal %s: %w", path, err)
	}
	events, end, err := parse(data)
	if err != nil {
		return nil, nil, fmt.Errorf("read journal %s: %w", path, err)
	}
	if end < len(data) {
		torn = data[end:]
	}
	return events, torn, nil
}

// ArchivePath is where Archive moves the session's journal, relative to the state directory; it fails when that
// archive already exists, so a caller can refuse before recording anything.
func (j *Journal) ArchivePath(session string) (string, error) {
	rel := filepath.Join(archiveDir, session+".jsonl")
	path := filepath.Join(j.dir, rel)
	for _, p := range []string{path, filepath.Join(j.dir, archiveDir, session+trialsSuffix)} {
		if _, err := os.Stat(p); err == nil {
			return "", fmt.Errorf("archive %s already exists", p)
		} else if !errors.Is(err, fs.ErrNotExist) {
			return "", fmt.Errorf("archive: %w", err)
		}
	}
	return rel, nil
}

// Archive records session.archived, then removes the state file and moves the journal to ArchivePath. The journal is
// spent afterwards: close it. Open finishes an archive that was recorded but not moved.
func (j *Journal) Archive(session string) (string, error) {
	rel, err := j.ArchivePath(session)
	if err != nil {
		return "", err
	}
	if _, err := j.Append(&SessionArchived{Session: session, Path: rel}); err != nil {
		return "", err
	}
	if j.opts.Sync {
		if err := j.f.Sync(); err != nil {
			return "", fmt.Errorf("sync %s: %w", KindSessionArchived, err)
		}
	}
	if err := finishArchive(j.dir, rel, j.opts.Sync); err != nil {
		return "", fmt.Errorf("archive: %w", err)
	}
	return rel, nil
}

const trialsSuffix = "-trials"

// finishArchive removes the state file and moves the trial directories before moving the journal: once the journal is
// gone nothing would finish the rest, while a journal still in place ends with session.archived and Open retries.
func finishArchive(dir, rel string, sync bool) error {
	archive := filepath.Join(dir, archiveDir)
	if err := os.MkdirAll(archive, 0o755); err != nil {
		return err
	}
	if err := os.Remove(filepath.Join(dir, stateFile)); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	session := strings.TrimSuffix(filepath.Base(rel), filepath.Ext(rel))
	if err := os.Rename(filepath.Join(dir, trialsDir), filepath.Join(archive, session+trialsSuffix)); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	if err := os.Rename(filepath.Join(dir, eventsFile), filepath.Join(dir, rel)); err != nil {
		return err
	}
	if sync {
		for _, d := range []string{archive, dir} {
			if err := syncDir(d); err != nil {
				return fmt.Errorf("sync directory %s: %w", d, err)
			}
		}
	}
	return nil
}

func Replay(events []Event, folders ...Folder) {
	for _, e := range events {
		for _, f := range folders {
			f.Fold(e)
		}
	}
}

func (j *Journal) Events() []Event {
	return j.events
}

func (j *Journal) Append(p Payload, cause ...int) (Event, error) {
	seq := len(j.events) + 1
	kind := p.Kind()
	if seq == 1 && kind != KindSessionStart {
		return Event{}, fmt.Errorf("append %s: the first event must be %s", kind, KindSessionStart)
	}
	for _, c := range cause {
		if c < 1 || c >= seq {
			return Event{}, fmt.Errorf("append %s: cause %d is not an earlier event", kind, c)
		}
	}
	e := Event{
		Seq:  seq,
		Time: j.opts.Now().UTC(),
		Boot: j.opts.Boot,
		Kind: kind,
		Msg:  p.Message(),
		Data: p,
	}
	if len(cause) > 0 {
		e.Cause = slices.Clone(cause)
	}
	raw, err := encode(e)
	if err != nil {
		return Event{}, fmt.Errorf("append %s: %w", kind, err)
	}
	line := append(raw, '\n')
	e.Raw = line[:len(line)-1]
	if _, err := j.f.Write(line); err != nil {
		return Event{}, fmt.Errorf("append %s: %w", kind, err)
	}
	if j.opts.Sync && (kind == KindSMUIntent || kind == KindTrialIntent) {
		if err := j.f.Sync(); err != nil {
			return Event{}, fmt.Errorf("sync %s: %w", kind, err)
		}
	}
	j.events = append(j.events, e)
	if j.opts.Log != nil {
		fmt.Fprintln(j.opts.Log, j.opts.Renderer.Line(e, time.Local))
	}
	return e, nil
}

func (j *Journal) Close() error {
	var errs []error
	if j.opts.Sync {
		errs = append(errs, j.f.Sync())
	}
	errs = append(errs, j.f.Close())
	errs = append(errs, syscall.Flock(int(j.lock.Fd()), syscall.LOCK_UN), j.lock.Close())
	if err := errors.Join(errs...); err != nil {
		return fmt.Errorf("close journal %s: %w", j.dir, err)
	}
	return nil
}
