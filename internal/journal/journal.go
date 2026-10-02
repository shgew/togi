package journal

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
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
	Boot      string
	Now       func() time.Time
	Monotonic func() time.Duration
	Sync      bool
	Log       io.Writer
	Renderer  Renderer
	Build     Build
}

var ErrLocked = errors.New("another togi process holds the journal lock")

type Journal struct {
	dir       string
	opts      Options
	lock      *os.File
	f         journalFile
	events    []Event
	fs        journalFilesystem
	appendErr error
}

type Folder interface {
	Fold(Event)
}

func Open(dir string, opts Options) (*Journal, error) {
	j, err := Lock(dir, opts)
	if err != nil {
		return nil, err
	}
	if err := j.Open(); err != nil {
		_ = j.Close()
		return nil, fmt.Errorf("open journal %s: %w", dir, err)
	}
	return j, nil
}

func Lock(dir string, opts Options) (*Journal, error) {
	if opts.Now == nil {
		opts.Now = time.Now
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, err
	}
	lock, err := lockOnly(dir)
	if err != nil {
		return nil, err
	}
	if err := os.Chmod(dir, 0o711); err != nil {
		_ = lock.Close()
		return nil, fmt.Errorf("set state directory traversal: %w", err)
	}
	return &Journal{dir: dir, opts: opts, lock: lock, fs: diskJournalFilesystem{}}, nil
}

func (j *Journal) Dir() string {
	return j.dir
}

func (j *Journal) Open() error {
	dir, opts := j.dir, j.opts
	path := filepath.Join(dir, eventsFile)
	data, err := os.ReadFile(path)
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	if errors.Is(err, fs.ErrNotExist) {
		if _, err := j.finishPendingArchive(); err != nil {
			return err
		}
	}
	events, end, err := parse(data, opts.Build)
	if err != nil {
		return err
	}
	if err := KnownKinds(events, opts.Build); err != nil {
		return err
	}
	if n := len(events); n > 0 && end == len(data) {
		if a, ok := events[n-1].Data.(*SessionArchived); ok {
			if err := j.finishArchive(a.Path); err != nil {
				return fmt.Errorf("finish archive: %w", err)
			}
			events, data, end = nil, nil, 0
		}
	}
	f, err := j.fs.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return err
	}
	if opts.Sync {
		for _, d := range []string{dir, filepath.Dir(dir)} {
			if err := j.fs.SyncDir(d); err != nil {
				f.Close()
				return fmt.Errorf("sync directory %s: %w", d, err)
			}
		}
	}
	j.f, j.events = f, events
	if end == len(data) {
		return nil
	}
	if err := f.Truncate(int64(end)); err != nil {
		return fmt.Errorf("truncate torn tail: %w", err)
	}
	if len(events) > 0 {
		torn := &JournalTorn{Offset: int64(end), BytesHex: hex.EncodeToString(data[end:])}
		if _, err := j.Append(torn); err != nil {
			return err
		}
	} else if opts.Sync {
		if err := f.Sync(); err != nil {
			return err
		}
	}
	return nil
}

func (j *Journal) RecoverPendingArchive() (string, error) {
	return j.finishPendingArchive()
}

func (j *Journal) DropPendingCarry() (string, error) {
	id, err := PendingCarry(j.dir)
	if err != nil || id == "" {
		return id, err
	}
	if err := j.MarkResetAll(); err != nil {
		return "", err
	}
	if err := j.ClearPendingCarry(); err != nil {
		return "", err
	}
	return id, nil
}

// lockOnly takes the writer lock without opening the journal; closing the file releases it.
func lockOnly(dir string) (*os.File, error) {
	lock, err := os.OpenFile(filepath.Join(dir, lockFile), os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return nil, fmt.Errorf("open journal lock: %w", err)
	}
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		lock.Close()
		if errors.Is(err, syscall.EWOULDBLOCK) {
			return nil, ErrLocked
		}
		return nil, fmt.Errorf("lock: %w", err)
	}
	return lock, nil
}

func pendingArchive(dir string) (string, error) {
	if _, err := os.Stat(filepath.Join(dir, eventsFile)); err == nil {
		return "", nil
	} else if !errors.Is(err, fs.ErrNotExist) {
		return "", fmt.Errorf("check journal for archive recovery: %w", err)
	}
	archive := filepath.Join(dir, archiveDir)
	entries, err := os.ReadDir(archive)
	if errors.Is(err, fs.ErrNotExist) {
		return "", nil
	}
	if err != nil {
		return "", fmt.Errorf("read archive markers: %w", err)
	}
	for _, entry := range entries {
		id, found := strings.CutSuffix(entry.Name(), "-compat-pending")
		if !found || id == "" {
			continue
		}
		if _, err := os.Stat(filepath.Join(archive, id+".jsonl")); err != nil {
			return "", fmt.Errorf("check pending archive %s: %w", id, err)
		}
		return id, nil
	}
	return "", nil
}

func (j *Journal) finishPendingArchive() (string, error) {
	dir := j.dir
	id, err := pendingArchive(dir)
	if err != nil || id == "" {
		return id, err
	}
	archive := filepath.Join(dir, archiveDir)
	for _, d := range []string{archive, dir} {
		if err := j.fs.SyncDir(d); err != nil {
			return "", fmt.Errorf("sync pending archive directory %s: %w", d, err)
		}
	}
	if err := j.fs.Remove(filepath.Join(archive, id+"-compat-pending")); err != nil {
		return "", fmt.Errorf("remove incompatible archive marker: %w", err)
	}
	if err := j.fs.SyncDir(archive); err != nil {
		return "", fmt.Errorf("sync incompatible archive completion: %w", err)
	}
	return id, nil
}

func parse(data []byte, binary Build) (events []Event, end int, err error) {
	return parseEvents(data, binary, false)
}

func parseEvents(data []byte, binary Build, history bool) (events []Event, end int, err error) {
	for {
		i := bytes.IndexByte(data[end:], '\n')
		if i < 0 {
			return events, end, nil
		}
		line := data[end : end+i]
		n := len(events) + 1
		if n == 1 {
			var first struct {
				Kind Kind `json:"kind"`
				Build
			}
			if err := json.Unmarshal(line, &first); err != nil {
				return nil, 0, fmt.Errorf("journal line 1: %w", err)
			}
			if history && (first.Schema < 1 || first.Schema > Schema) {
				return nil, 0, fmt.Errorf("journal schema %d cannot be read by schema %d", first.Schema, Schema)
			}
			if first.Kind == KindSessionStart && !history {
				if binary.Schema == 0 {
					binary = binarySchemaBuild()
				}
				recordedRuleset := first.Ruleset
				if recordedRuleset == 0 {
					recordedRuleset = 1
				}
				if binary.Ruleset == 0 {
					binary.Ruleset = recordedRuleset
				}
				if first.Schema != binary.Schema || recordedRuleset != binary.Ruleset {
					stamp, _, err := scanBuild(data)
					if err != nil {
						return nil, 0, err
					}
					return nil, 0, Compatible(stamp, binary)
				}
			}
		}
		e, err := decodeEvent(line, history)
		if err != nil {
			return nil, 0, fmt.Errorf("journal line %d: %w", n, err)
		}
		if e.Seq != n {
			return nil, 0, fmt.Errorf("journal line %d: seq %d, want %d", n, e.Seq, n)
		}
		if n == 1 {
			if _, ok := e.Data.(*SessionStart); !ok {
				return nil, 0, fmt.Errorf("journal line 1: first event is %s, want %s", e.Kind, KindSessionStart)
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
	events, end, err := parse(data, Build{})
	if err != nil {
		return nil, nil, fmt.Errorf("read journal %s: %w", path, err)
	}
	if end < len(data) {
		torn = data[end:]
	}
	return events, torn, nil
}

// ReadHistory reads all understood events from shipped schemas without enforcing
// resume compatibility. Unknown kinds remain opaque and a torn tail is ignored.
// ConfigLoaded retains only its build stamp, not its historical configuration.
func ReadHistory(path string) ([]Event, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read journal %s: %w", path, err)
	}
	events, _, err := parseEvents(data, Build{}, true)
	if err != nil {
		return nil, fmt.Errorf("read journal %s: %w", path, err)
	}
	return events, nil
}

var carryKinds = map[Kind]bool{
	KindSessionStart:   true,
	KindSessionContext: true,
	KindSessionCarried: true,
	KindTrialCarried:   true,
	KindFailureCarried: true,
	KindProfileApplied: true,
	KindProfileChange:  true,
	KindSMUReadback:    true,
	KindTrialIntent:    true,
	KindTrialEnd:       true,
	KindTrialProgress:  true,
	KindFailure:        true,
	KindHuntStart:      true,
	KindHuntEnd:        true,
	KindCommandReset:   true,
	KindShutdown:       true,
	KindTunerDecision:  true,
	KindDefectFound:    true,
}

// ReadForCarry decodes, from a journal of any schema that shipped, only the kinds a carry reads.
func ReadForCarry(path string) ([]Event, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read journal %s: %w", path, err)
	}
	var events []Event
	for n := 1; ; n++ {
		line, rest, ok := bytes.Cut(data, []byte{'\n'})
		if !ok {
			break
		}
		data = rest
		var env envelope
		if err := json.Unmarshal(line, &env); err != nil {
			return nil, fmt.Errorf("read journal %s line %d: %w", path, n, err)
		}
		if env.Kind == "" {
			return nil, fmt.Errorf("read journal %s line %d: event has no kind", path, n)
		}
		if n == 1 && env.Kind != KindSessionStart {
			return nil, fmt.Errorf("read journal %s line 1: first event is %s, want %s", path, env.Kind, KindSessionStart)
		}
		var p Payload
		switch {
		case carryKinds[env.Kind]:
			if p, err = decodePayload(env.Kind, line); err != nil {
				return nil, fmt.Errorf("read journal %s line %d: %w", path, n, err)
			}
		case env.Kind == KindConfigLoaded:
			var b Build
			if err := json.Unmarshal(line, &b); err != nil {
				return nil, fmt.Errorf("read journal %s line %d: %w", path, n, err)
			}
			p = &ConfigLoaded{Build: b}
		default:
			continue
		}
		events = append(events, Event{Seq: env.Seq, Time: env.Time, Mono: env.Mono, Boot: env.Boot, Kind: env.Kind, Msg: env.Msg, Cause: env.Cause, Data: p, Raw: line})
	}
	if len(events) == 0 {
		return nil, fmt.Errorf("read journal %s: no session.start", path)
	}
	return events, nil
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
	if err := j.finishArchive(rel); err != nil {
		return "", fmt.Errorf("archive: %w", err)
	}
	return rel, nil
}

func (j *Journal) ArchiveUnreadable(session string) (string, error) {
	rel := filepath.Join(archiveDir, session+".jsonl")
	// A previous build may already have recorded session.archived before the update.
	data, err := os.ReadFile(filepath.Join(j.dir, eventsFile))
	if err != nil {
		return "", fmt.Errorf("read incompatible journal for archive recovery: %w", err)
	}
	if len(data) > 0 && data[len(data)-1] == '\n' {
		end := len(data) - 1
		start := bytes.LastIndexByte(data[:end], '\n') + 1
		var last struct {
			Kind    Kind   `json:"kind"`
			Path    string `json:"path"`
			Session string `json:"session"`
		}
		if json.Unmarshal(data[start:end], &last) == nil && last.Kind == KindSessionArchived && last.Session == session && last.Path == rel {
			if err := j.finishArchive(rel); err != nil {
				return "", fmt.Errorf("finish recorded incompatible archive: %w", err)
			}
			return rel, nil
		}
	}
	archive := filepath.Join(j.dir, archiveDir)
	pending := filepath.Join(archive, session+"-compat-pending")
	_, pendingErr := os.Stat(pending)
	switch {
	case errors.Is(pendingErr, fs.ErrNotExist):
		if _, err := j.ArchivePath(session); err != nil {
			return "", err
		}
		if err := os.MkdirAll(archive, 0o755); err != nil {
			return "", fmt.Errorf("create archive directory: %w", err)
		}
		marker, err := j.fs.OpenFile(pending, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o644)
		if err != nil {
			return "", fmt.Errorf("mark incompatible archive pending: %w", err)
		}
		if j.opts.Sync {
			if err := marker.Sync(); err != nil {
				_ = marker.Close()
				return "", fmt.Errorf("sync incompatible archive marker: %w", err)
			}
		}
		if err := marker.Close(); err != nil {
			return "", fmt.Errorf("close incompatible archive marker: %w", err)
		}
		if j.opts.Sync {
			if err := j.fs.SyncDir(archive); err != nil {
				return "", fmt.Errorf("sync incompatible archive marker: %w", err)
			}
		}
	case pendingErr != nil:
		return "", fmt.Errorf("check incompatible archive marker: %w", pendingErr)
	}
	if _, err := os.Stat(filepath.Join(j.dir, rel)); err == nil {
		return "", fmt.Errorf("archive %s already exists", filepath.Join(j.dir, rel))
	} else if !errors.Is(err, fs.ErrNotExist) {
		return "", fmt.Errorf("check archive: %w", err)
	}
	if err := j.finishArchive(rel); err != nil {
		return "", fmt.Errorf("archive incompatible journal: %w", err)
	}
	if err := j.fs.Remove(pending); err != nil {
		return "", fmt.Errorf("remove incompatible archive marker: %w", err)
	}
	if j.opts.Sync {
		if err := j.fs.SyncDir(archive); err != nil {
			return "", fmt.Errorf("sync incompatible archive completion: %w", err)
		}
	}
	return rel, nil
}

const carrySuffix = "-carry-pending"

func (j *Journal) ArchiveForCarry(session string) (string, error) {
	data, err := os.ReadFile(filepath.Join(j.dir, eventsFile))
	if err != nil {
		return "", fmt.Errorf("read journal for carry: %w", err)
	}
	if len(data) == 0 {
		return "", nil
	}
	if end := bytes.LastIndexByte(data, '\n'); end > 0 {
		start := bytes.LastIndexByte(data[:end], '\n') + 1
		var last struct {
			Kind Kind `json:"kind"`
		}
		if json.Unmarshal(data[start:end], &last) == nil && last.Kind == KindSessionArchived {
			if _, err := j.ArchiveUnreadable(session); err != nil {
				return "", err
			}
			return "", nil
		}
	}
	pending, err := PendingCarry(j.dir)
	if err != nil {
		return "", err
	}
	if pending != "" && !carryEstablished(data) {
		return j.ArchiveUnreadable(session)
	}
	if err := j.ClearPendingCarry(); err != nil {
		return "", err
	}
	archive := filepath.Join(j.dir, archiveDir)
	if err := os.MkdirAll(archive, 0o755); err != nil {
		return "", fmt.Errorf("create archive directory: %w", err)
	}
	marker, err := j.fs.OpenFile(filepath.Join(archive, session+carrySuffix), os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return "", fmt.Errorf("mark carry pending: %w", err)
	}
	if j.opts.Sync {
		if err := marker.Sync(); err != nil {
			_ = marker.Close()
			return "", fmt.Errorf("sync carry marker: %w", err)
		}
	}
	if err := marker.Close(); err != nil {
		return "", fmt.Errorf("close carry marker: %w", err)
	}
	if j.opts.Sync {
		if err := j.fs.SyncDir(archive); err != nil {
			return "", fmt.Errorf("sync carry marker: %w", err)
		}
	}
	return j.ArchiveUnreadable(session)
}

func carryEstablished(data []byte) bool {
	for len(data) > 0 {
		end := bytes.IndexByte(data, '\n')
		if end < 0 {
			break
		}
		var event struct {
			Kind Kind `json:"kind"`
		}
		if json.Unmarshal(data[:end], &event) == nil && (event.Kind == KindSessionContext || event.Kind == KindSessionCarried) {
			return true
		}
		data = data[end+1:]
	}
	return false
}

// PendingCarry returns the session whose carry no journal has recorded yet, or "" when there is none.
func PendingCarry(dir string) (string, error) {
	entries, err := os.ReadDir(filepath.Join(dir, archiveDir))
	if errors.Is(err, fs.ErrNotExist) {
		return "", nil
	}
	if err != nil {
		return "", fmt.Errorf("read carry markers: %w", err)
	}
	var pending string
	for _, entry := range entries {
		if id, found := strings.CutSuffix(entry.Name(), carrySuffix); found && CompareSessionIDs(id, pending) > 0 {
			pending = id
		}
	}
	return pending, nil
}

func (j *Journal) ClearPendingCarry() error {
	archive := filepath.Join(j.dir, archiveDir)
	entries, err := os.ReadDir(archive)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("read carry markers: %w", err)
	}
	for _, entry := range entries {
		if !strings.HasSuffix(entry.Name(), carrySuffix) {
			continue
		}
		if err := j.fs.Remove(filepath.Join(archive, entry.Name())); err != nil && !errors.Is(err, fs.ErrNotExist) {
			return fmt.Errorf("remove carry marker: %w", err)
		}
	}
	if err := j.fs.SyncDir(archive); err != nil {
		return fmt.Errorf("sync carry markers: %w", err)
	}
	return nil
}

const trialsSuffix = "-trials"

func (j *Journal) finishArchive(rel string) error {
	dir := j.dir
	archive := filepath.Join(dir, archiveDir)
	if err := os.MkdirAll(archive, 0o755); err != nil {
		return err
	}
	if err := j.fs.Remove(filepath.Join(dir, stateFile)); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	session := strings.TrimSuffix(filepath.Base(rel), filepath.Ext(rel))
	if err := j.fs.Rename(filepath.Join(dir, trialsDir), filepath.Join(archive, session+trialsSuffix)); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	if j.opts.Sync {
		for _, d := range []string{archive, dir} {
			if err := j.fs.SyncDir(d); err != nil {
				return fmt.Errorf("sync trial archive directory %s: %w", d, err)
			}
		}
	}
	if err := j.fs.Rename(filepath.Join(dir, eventsFile), filepath.Join(dir, rel)); err != nil {
		return err
	}
	if j.opts.Sync {
		for _, d := range []string{archive, dir} {
			if err := j.fs.SyncDir(d); err != nil {
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
	if j.appendErr != nil {
		return Event{}, j.appendErr
	}
	seq := len(j.events) + 1
	kind := p.Kind()
	if _, ok := payloadConstructors[kind]; !ok {
		return Event{}, fmt.Errorf("append %s: unregistered kind", kind)
	}
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
	if j.opts.Monotonic != nil {
		e.Mono = j.opts.Monotonic().Milliseconds()
	}
	if len(cause) > 0 {
		e.Cause = slices.Clone(cause)
	}
	raw, err := encode(e, j.opts.Monotonic != nil)
	if err != nil {
		return Event{}, fmt.Errorf("append %s: %w", kind, err)
	}
	raw = append(raw, '\n')
	e.Raw = raw[:len(raw)-1]
	n, err := j.f.Write(raw)
	if err == nil && n != len(raw) {
		err = io.ErrShortWrite
	}
	if err != nil {
		j.appendErr = fmt.Errorf("append %s: %w", kind, err)
		return Event{}, j.appendErr
	}
	if j.opts.Sync {
		if err := j.f.Sync(); err != nil {
			j.appendErr = fmt.Errorf("sync %s: %w", kind, err)
			return Event{}, j.appendErr
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
	if j.f != nil {
		if j.opts.Sync {
			errs = append(errs, j.f.Sync())
		}
		errs = append(errs, j.f.Close())
		j.f = nil
	}
	if j.lock != nil {
		errs = append(errs, syscall.Flock(int(j.lock.Fd()), syscall.LOCK_UN), j.lock.Close())
		j.lock = nil
	}
	if err := errors.Join(errs...); err != nil {
		return fmt.Errorf("close journal %s: %w", j.dir, err)
	}
	return nil
}
