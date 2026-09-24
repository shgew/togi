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
	"syscall"
	"time"
)

const (
	eventsFile = "events.jsonl"
	lockFile   = "lock"
)

type Options struct {
	Boot string
	Now  func() time.Time
	Sync bool
	Log  io.Writer
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
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		lock.Close()
		return nil, err
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
	data, err := os.ReadFile(filepath.Join(dir, eventsFile))
	if err != nil {
		return nil, nil, fmt.Errorf("read journal %s: %w", dir, err)
	}
	events, end, err := parse(data)
	if err != nil {
		return nil, nil, fmt.Errorf("read journal %s: %w", dir, err)
	}
	if end < len(data) {
		torn = data[end:]
	}
	return events, torn, nil
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
		fmt.Fprintln(j.opts.Log, FormatLine(e, time.Local))
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
