package journal

import (
	"bufio"
	"bytes"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"sync"
	"syscall"
	"time"
)

const (
	eventsFile    = "events.jsonl"
	eventsTmpFile = ".events.jsonl.tmp"
	lockFile      = "lock"
	archiveDir    = "archive"
	trialsDir     = "trials"
	// bufferSize is the size of a buffered journal's write buffer.
	bufferSize = 64 << 10
)

type Options struct {
	Boot      string
	Now       func() time.Time
	Monotonic func() time.Duration
	Sync      bool
	// Buffered holds appended lines in memory until the journal is closed or something reads the file back. A crash
	// of the process loses them, so only a simulator that never crashes its own process sets it; it cannot be combined
	// with Sync.
	Buffered bool
	Build    Build
	// Prefix, shared by every Lock of one journal, lets a simulation that reopens it each boot decode only the lines appended since.
	Prefix *Prefix
}

// Prefix holds the bytes and events of the last decode of a journal; a file that no longer starts with those bytes is decoded whole.
type Prefix struct {
	data   []byte
	events []Event
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
	decoded   *decodedFile
	// w buffers appends to f when Options.Buffered is set.
	w *bufio.Writer
}

type decodedFile struct {
	data   []byte
	events []Event
	end    int
}

type Folder interface {
	Fold(Event)
}

func Open(dir string, opts Options) (*Journal, error) {
	j, err := Lock(dir, opts)
	if err != nil {
		return nil, err
	}
	if _, err := j.Open(); err != nil {
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

// flush writes the appends a buffered journal still holds, so that the file holds every event before anything reads it.
func (j *Journal) flush() error {
	if j.w == nil {
		return nil
	}
	if err := j.w.Flush(); err != nil {
		j.appendErr = fmt.Errorf("flush journal: %w", err)
		return j.appendErr
	}
	return nil
}

func (j *Journal) SetBoot(boot string) {
	j.opts.Boot = boot
}

// Read decodes the locked journal like Read(j.Dir()); Open reuses the decoded events while the file is unchanged, and
// so does another Read. The returned events are shared with that cache and must not be modified.
func (j *Journal) Read() (events []Event, torn []byte, err error) {
	if err := j.flush(); err != nil {
		return nil, nil, err
	}
	path := filepath.Join(j.dir, eventsFile)
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, nil, fmt.Errorf("read journal %s: %w", path, err)
	}
	if d := j.decoded; d != nil && bytes.Equal(d.data, data) {
		if err := checkStart(data, Build{}, readLive); err != nil {
			return nil, nil, fmt.Errorf("read journal %s: %w", path, err)
		}
		return d.events, tornTail(data, d.end), nil
	}
	events, end, err := j.decode(data, Build{})
	if err != nil {
		return nil, nil, fmt.Errorf("read journal %s: %w", path, err)
	}
	j.decoded = &decodedFile{data: data, events: events, end: end}
	return events, tornTail(data, end), nil
}

func (j *Journal) decode(data []byte, binary Build) ([]Event, int, error) {
	if err := checkStart(data, binary, readLive); err != nil {
		return nil, 0, err
	}
	p := j.opts.Prefix
	if p == nil {
		return decodeLines(data, readLive)
	}
	if len(p.data) == 0 || !bytes.HasPrefix(data, p.data) {
		events, end, err := decodeLines(data, readLive)
		if err != nil {
			return nil, 0, err
		}
		p.data, p.events = data[:end], slices.Clip(events)
		return events, end, nil
	}
	tail, end, err := decodeLinesAfter(data[len(p.data):], readLive, len(p.events))
	if err != nil {
		return nil, 0, err
	}
	// Point earlier events at this read's identical bytes so earlier reads' buffers can be freed.
	events := make([]Event, len(p.events), len(p.events)+len(tail))
	off := 0
	for i, e := range p.events {
		e.Raw = data[off : off+len(e.Raw)]
		off += len(e.Raw) + 1
		events[i] = e
	}
	events = append(events, tail...)
	end += len(p.data)
	p.data, p.events = data[:end], slices.Clip(events)
	return events, end, nil
}

func (j *Journal) parse(data []byte) ([]Event, int, error) {
	decoded := j.decoded
	j.decoded = nil
	if decoded != nil && bytes.Equal(decoded.data, data) {
		if err := checkStart(data, j.opts.Build, readLive); err != nil {
			return nil, 0, err
		}
		return slices.Clone(decoded.events), decoded.end, nil
	}
	return j.decode(data, j.opts.Build)
}

// Open reads the journal and opens it for appending. A torn tail is replaced by a journal.torn event, which Open
// returns once it is persisted so the caller can log it.
func (j *Journal) Open() (torn []Event, err error) {
	dir, opts := j.dir, j.opts
	path := filepath.Join(dir, eventsFile)
	data, err := os.ReadFile(path)
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return nil, err
	}
	if errors.Is(err, fs.ErrNotExist) {
		if _, err := j.finishPendingArchive(); err != nil {
			return nil, err
		}
	}
	events, end, err := j.parse(data)
	if err != nil {
		return nil, err
	}
	if err := Classify(BuildOf(events), opts.Build).Kinds(OpAppend, events); err != nil {
		return nil, err
	}
	if n := len(events); n > 0 && end == len(data) {
		if a, ok := events[n-1].Data.(*SessionArchived); ok {
			if err := j.finishArchive(a.Path); err != nil {
				return nil, fmt.Errorf("finish archive: %w", err)
			}
			events, data, end = nil, nil, 0
		}
	}
	if end < len(data) && len(events) > 0 {
		e, err := j.replaceTornTail(path, data[:end], len(events)+1, data[end:])
		if err != nil {
			return nil, err
		}
		torn = []Event{e}
		events, data, end = append(events, e), nil, 0
	}
	if opts.Buffered && opts.Sync {
		return nil, errors.New("open journal: a buffered journal cannot sync each append")
	}
	f, err := j.fs.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return nil, err
	}
	if opts.Sync {
		for _, d := range []string{dir, filepath.Dir(dir)} {
			if err := j.fs.SyncDir(d); err != nil {
				f.Close()
				return nil, fmt.Errorf("sync directory %s: %w", d, err)
			}
		}
	}
	j.f, j.events = f, events
	if opts.Buffered {
		j.w = bufio.NewWriterSize(f, bufferSize)
	}
	if end == len(data) {
		return torn, nil
	}
	if err := f.Truncate(int64(end)); err != nil {
		return nil, fmt.Errorf("truncate torn first line: %w", err)
	}
	if opts.Sync {
		if err := f.Sync(); err != nil {
			return nil, fmt.Errorf("sync truncated torn first line: %w", err)
		}
	}
	return nil, nil
}

// replaceTornTail puts in place of the journal its complete lines followed by a journal.torn event holding the torn
// tail. Until the rename the journal keeps the torn tail, so an interrupted repair is redone by the next Open and
// records the tail exactly once.
func (j *Journal) replaceTornTail(path string, complete []byte, seq int, tail []byte) (Event, error) {
	e, raw, err := j.next(seq, &JournalTorn{Offset: int64(len(complete)), BytesHex: hex.EncodeToString(tail)}, nil)
	if err != nil {
		return Event{}, err
	}
	tmp := filepath.Join(j.dir, eventsTmpFile)
	if err := j.writeTemp(tmp, append(slices.Clip(complete), raw...)); err != nil {
		_ = j.fs.Remove(tmp)
		return Event{}, fmt.Errorf("write repaired journal: %w", err)
	}
	if err := j.fs.Rename(tmp, path); err != nil {
		_ = j.fs.Remove(tmp)
		return Event{}, fmt.Errorf("replace journal with repaired journal: %w", err)
	}
	return e, nil
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

type readMode uint8

const (
	readLive readMode = iota
	readHistory
	readReplay
)

func parse(data []byte, binary Build) (events []Event, end int, err error) {
	return parseEvents(data, binary, readLive)
}

func parseEvents(data []byte, binary Build, mode readMode) (events []Event, end int, err error) {
	if err := checkStart(data, binary, mode); err != nil {
		return nil, 0, err
	}
	return decodeLines(data, mode)
}

// checkStart refuses a journal whose session.start this binary cannot resume, before any other line is decoded.
func checkStart(data []byte, binary Build, mode readMode) error {
	line, _, ok := bytes.Cut(data, []byte{'\n'})
	if !ok {
		return nil
	}
	var first struct {
		Kind Kind `json:"kind"`
		Build
	}
	if err := json.Unmarshal(line, &first); err != nil {
		return fmt.Errorf("journal line 1: %w", err)
	}
	if mode != readLive && !shippedSchema(first.Build) {
		return fmt.Errorf("journal schema %d cannot be read by schema %d", first.Schema, Schema)
	}
	if first.Kind != KindSessionStart || mode == readHistory {
		return nil
	}
	if binary.Schema == 0 {
		binary = binarySchemaBuild()
	}
	c := Classify(first.Build, binary)
	if mode == readReplay {
		if c.Ruleset != DirSame {
			return fmt.Errorf("journal ruleset %d cannot be replayed by ruleset %d; replay needs a journal from the current ruleset", c.Journal.Ruleset, binary.Ruleset)
		}
		return nil
	}
	if binary.Ruleset == 0 {
		binary.Ruleset = c.Journal.Ruleset
		c = Classify(first.Build, binary)
	}
	if c.Schema != DirSame || c.Ruleset != DirSame {
		stamp, _, err := scanBuild(data)
		if err != nil {
			return err
		}
		return Classify(stamp, binary).Err()
	}
	return nil
}

// minDecodePart keeps small journals on one goroutine, where splitting costs more than it saves.
const minDecodePart = 256 << 10

func decodeLines(data []byte, mode readMode) (events []Event, end int, err error) {
	return decodeLinesAfter(data, mode, 0)
}

// decodeLinesAfter decodes lines that follow the first `before` events of a journal.
func decodeLinesAfter(data []byte, mode readMode, before int) (events []Event, end int, err error) {
	end = bytes.LastIndexByte(data, '\n') + 1
	if end == 0 {
		return nil, 0, nil
	}
	schema := Schema
	if mode != readLive {
		line, _, _ := bytes.Cut(data, []byte{'\n'})
		var build Build
		if err := json.Unmarshal(line, &build); err != nil {
			return nil, 0, err
		}
		schema = build.Schema
	}
	events, err = decodeParts(lineParts(data[:end], runtime.GOMAXPROCS(0), minDecodePart), mode == readHistory, schema, before)
	if err != nil {
		return nil, 0, err
	}
	return events, end, nil
}

// lineParts splits complete lines into about n parts of at least least bytes, each ending at a line end.
func lineParts(data []byte, n, least int) [][]byte {
	size := max(len(data)/n, least)
	var parts [][]byte
	for len(data) > size {
		cut := size + bytes.IndexByte(data[size:], '\n') + 1
		parts = append(parts, data[:cut])
		data = data[cut:]
	}
	if len(data) > 0 {
		parts = append(parts, data)
	}
	return parts
}

type decodedPart struct {
	events []Event
	err    error
}

// decodeParts decodes parts concurrently, then checks them in line order so the first bad line is the one reported.
func decodeParts(parts [][]byte, history bool, schema, before int) ([]Event, error) {
	decoded := make([]decodedPart, len(parts))
	if len(parts) == 1 {
		decoded[0] = decodePart(parts[0], history, schema)
	} else {
		var wg sync.WaitGroup
		for i, part := range parts {
			wg.Go(func() { decoded[i] = decodePart(part, history, schema) })
		}
		wg.Wait()
	}
	n := before
	for _, d := range decoded {
		for _, e := range d.events {
			n++
			if e.Seq != n {
				return nil, fmt.Errorf("journal line %d: seq %d, want %d", n, e.Seq, n)
			}
			if n == 1 {
				if _, ok := e.Data.(*SessionStart); !ok {
					return nil, fmt.Errorf("journal line 1: first event is %s, want %s", e.Kind, KindSessionStart)
				}
			}
		}
		if d.err != nil {
			return nil, fmt.Errorf("journal line %d: %w", n+1, d.err)
		}
	}
	if len(decoded) == 1 {
		return decoded[0].events, nil
	}
	events := make([]Event, 0, n)
	for _, d := range decoded {
		events = append(events, d.events...)
	}
	return events, nil
}

// decodePart decodes lines until the first that fails.
func decodePart(part []byte, history bool, schema int) decodedPart {
	var d decodedPart
	olderSchema := Classify(Build{Schema: schema}, Build{Schema: Schema}).Schema == DirOlder
	for len(part) > 0 {
		i := bytes.IndexByte(part, '\n')
		line := part[:i]
		if olderSchema {
			var err error
			line, err = translateSchema(line, schema)
			if err != nil {
				d.err = err
				return d
			}
		}
		e, err := decodeEvent(line, history)
		if err != nil {
			d.err = err
			return d
		}
		e.Raw = part[:i]
		d.events = append(d.events, e)
		part = part[i+1:]
	}
	return d
}

func Read(dir string) (events []Event, torn []byte, err error) {
	return ReadFile(filepath.Join(dir, eventsFile))
}

func ReadFile(path string) (events []Event, torn []byte, err error) {
	data, events, end, err := decodeFile(path, Build{}, readLive)
	if err != nil {
		return nil, nil, err
	}
	return events, tornTail(data, end), nil
}

func decodeFile(path string, binary Build, mode readMode) (data []byte, events []Event, end int, err error) {
	data, err = os.ReadFile(path)
	if err != nil {
		return nil, nil, 0, fmt.Errorf("read journal %s: %w", path, err)
	}
	events, end, err = parseEvents(data, binary, mode)
	if err != nil {
		return nil, nil, 0, fmt.Errorf("read journal %s: %w", path, err)
	}
	return data, events, end, nil
}

func tornTail(data []byte, end int) []byte {
	if end == len(data) {
		return nil
	}
	return data[end:]
}

// ReadHistory reads all understood events from shipped schemas without enforcing
// resume compatibility. Unknown kinds remain opaque and a torn tail is ignored.
// ConfigLoaded retains only its build stamp and backend store paths, not the rest of its historical configuration.
func ReadHistory(path string) ([]Event, error) {
	_, events, _, err := decodeFile(path, Build{}, readHistory)
	if err != nil {
		return nil, err
	}
	return events, nil
}

func ReadReplay(dir string, ruleset int) (events []Event, torn []byte, err error) {
	data, events, end, err := decodeFile(filepath.Join(dir, eventsFile), Build{Schema: Schema, Ruleset: ruleset}, readReplay)
	if err != nil {
		return nil, nil, err
	}
	return events, tornTail(data, end), nil
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
	schema := Schema
	olderSchema := false
	for n := 1; ; n++ {
		line, rest, ok := bytes.Cut(data, []byte{'\n'})
		if !ok {
			break
		}
		data = rest
		raw := line
		if n == 1 {
			var build Build
			if err := json.Unmarshal(line, &build); err != nil {
				return nil, fmt.Errorf("read journal %s line %d: %w", path, n, err)
			}
			schema = build.Schema
			if !shippedSchema(build) {
				return nil, fmt.Errorf("journal schema %d cannot be read by schema %d", schema, Schema)
			}
			olderSchema = Classify(build, Build{Schema: Schema}).Schema == DirOlder
		}
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
		if schema < 3 {
			if kind := legacyKinds[string(env.Kind)]; kind != "" {
				env.Kind = Kind(kind)
			}
		}
		if schema < 4 {
			if kind := cycleKinds[string(env.Kind)]; kind != "" {
				env.Kind = Kind(kind)
			}
		}
		var p Payload
		switch {
		case carryKinds[env.Kind]:
			if olderSchema {
				line, err = translateSchema(line, schema)
				if err != nil {
					return nil, fmt.Errorf("read journal %s line %d: %w", path, n, err)
				}
			}
			if p, err = decodePayload(env.Kind, line); err != nil {
				return nil, fmt.Errorf("read journal %s line %d: %w", path, n, err)
			}
		case env.Kind == KindConfigLoaded:
			if p, err = historicConfig(line); err != nil {
				return nil, fmt.Errorf("read journal %s line %d: %w", path, n, err)
			}
		default:
			continue
		}
		events = append(events, Event{Seq: env.Seq, Time: env.Time, Mono: env.Mono, Boot: env.Boot, Kind: env.Kind, Msg: env.Msg, Cause: env.Cause, Data: p, Raw: raw})
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
	if err := j.flush(); err != nil {
		return "", err
	}
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
	if err := j.flush(); err != nil {
		return "", err
	}
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

// ArchivedSessions lists the sessions archived in dir, oldest first. Every entry named <session>.jsonl counts, whatever its type;
// a missing archive directory lists none.
func ArchivedSessions(dir string) ([]string, error) {
	entries, err := os.ReadDir(filepath.Join(dir, archiveDir))
	if errors.Is(err, fs.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("list archived sessions: %w", err)
	}
	var sessions []string
	for _, entry := range entries {
		if id, found := strings.CutSuffix(entry.Name(), ".jsonl"); found {
			sessions = append(sessions, id)
		}
	}
	slices.SortFunc(sessions, CompareSessionIDs)
	return sessions, nil
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
	if err := j.flush(); err != nil {
		return err
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
	e, raw, err := j.next(len(j.events)+1, p, cause)
	if err != nil {
		return Event{}, err
	}
	kind := e.Kind
	var w io.Writer = j.f
	if j.w != nil {
		w = j.w
	}
	n, err := w.Write(raw)
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
	return e, nil
}

// next builds event seq and its line with the newline.
func (j *Journal) next(seq int, p Payload, cause []int) (Event, []byte, error) {
	kind := p.Kind()
	if _, ok := payloadTypes[kind]; !ok {
		return Event{}, nil, fmt.Errorf("append %s: unregistered kind", kind)
	}
	if seq == 1 && kind != KindSessionStart {
		return Event{}, nil, fmt.Errorf("append %s: the first event must be %s", kind, KindSessionStart)
	}
	for _, c := range cause {
		if c < 1 || c >= seq {
			return Event{}, nil, fmt.Errorf("append %s: cause %d is not an earlier event", kind, c)
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
		return Event{}, nil, fmt.Errorf("append %s: %w", kind, err)
	}
	raw = append(raw, '\n')
	e.Raw = raw[:len(raw)-1]
	return e, raw, nil
}

func (j *Journal) Close() error {
	var errs []error
	if j.w != nil {
		errs = append(errs, j.w.Flush())
		j.w = nil
	}
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
