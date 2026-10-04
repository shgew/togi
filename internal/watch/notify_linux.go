package watch

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"golang.org/x/sys/unix"
)

const journalWatchMask = unix.IN_MODIFY | unix.IN_CLOSE_WRITE | unix.IN_ATTRIB | unix.IN_CREATE | unix.IN_MOVED_FROM | unix.IN_MOVED_TO | unix.IN_DELETE | unix.IN_DELETE_SELF | unix.IN_MOVE_SELF | unix.IN_ONLYDIR

const journalFileMask = unix.IN_MODIFY | unix.IN_CLOSE_WRITE | unix.IN_ATTRIB | unix.IN_DELETE_SELF | unix.IN_MOVE_SELF

type journalNotifier struct {
	fd      int
	dir     string
	watches map[int]string
	// polling is set while the state directory cannot be watched and holds no journal to watch instead,
	// so nothing would announce the journal's creation.
	polling bool
}

func (n *journalNotifier) journal() string { return filepath.Join(n.dir, "events.jsonl") }

func (n *journalNotifier) arm() error {
	watches := make(map[int]string, 2)
	polling := false
	path := n.dir
climb:
	for {
		wd, err := unix.InotifyAddWatch(n.fd, path, journalWatchMask)
		switch {
		case err == nil:
			watches[wd] = path
			if path != n.dir || path == filepath.Dir(path) {
				break climb
			}
		case path == n.dir && errors.Is(err, unix.EACCES):
			// A root run leaves the state directory traverse-only (0711): other users can read the journal but
			// not watch the directory, so they watch the journal itself.
			wd, err := unix.InotifyAddWatch(n.fd, n.journal(), journalFileMask)
			switch {
			case err == nil:
				watches[wd] = n.journal()
			case errors.Is(err, unix.ENOENT):
				polling = true
			default:
				return fmt.Errorf("watch journal: %w", err)
			}
		case path != n.dir && errors.Is(err, unix.EACCES):
			// An unreadable parent only costs noticing the state directory's replacement, unless nothing below it
			// could be watched either.
			polling = polling || len(watches) == 0
			break climb
		case !errors.Is(err, unix.ENOENT) && !errors.Is(err, unix.ENOTDIR):
			return fmt.Errorf("watch state directory: %w", err)
		}
		parent := filepath.Dir(path)
		if parent == path {
			if polling || len(watches) > 0 {
				break
			}
			return fmt.Errorf("watch state directory: %w", unix.ENOENT)
		}
		path = parent
	}
	for wd := range n.watches {
		if _, keep := watches[wd]; !keep {
			_, _ = unix.InotifyRmWatch(n.fd, uint32(wd))
		}
	}
	n.watches = watches
	n.polling = polling
	return nil
}

func (n *journalNotifier) relevant(data []byte) bool {
	changed := false
	for len(data) >= unix.SizeofInotifyEvent {
		wd := int(int32(binary.NativeEndian.Uint32(data)))
		mask := binary.NativeEndian.Uint32(data[4:])
		length := int(binary.NativeEndian.Uint32(data[12:]))
		end := unix.SizeofInotifyEvent + length
		if end > len(data) {
			break
		}
		name := string(bytes.TrimRight(data[unix.SizeofInotifyEvent:end], "\x00"))
		data = data[end:]
		if mask&unix.IN_Q_OVERFLOW != 0 {
			changed = true
			continue
		}
		path, watched := n.watches[wd]
		if !watched {
			continue
		}
		if mask&(unix.IN_IGNORED|unix.IN_DELETE_SELF|unix.IN_MOVE_SELF) != 0 || path == n.journal() {
			changed = true
			continue
		}
		if path == n.dir {
			changed = changed || name == "events.jsonl"
			continue
		}
		rel, err := filepath.Rel(path, n.dir)
		if err == nil && name == strings.SplitN(rel, string(filepath.Separator), 2)[0] {
			changed = true
		}
	}
	return changed
}

// watchJournal arms notifications, then calls load for the first frame. A journal that appeared in between is
// watched before the reader starts, so discovery polling never runs while a loaded journal is on screen.
func watchJournal(ctx context.Context, dir string, load func()) (<-chan error, func(), error) {
	dir, err := filepath.Abs(dir)
	if err != nil {
		return nil, nil, fmt.Errorf("resolve state directory: %w", err)
	}
	fd, err := unix.InotifyInit1(unix.IN_CLOEXEC | unix.IN_NONBLOCK)
	if err != nil {
		return nil, nil, fmt.Errorf("open journal notifications: %w", err)
	}
	file := os.NewFile(uintptr(fd), "journal notifications")
	n := journalNotifier{fd: fd, dir: dir}
	if err := n.arm(); err != nil {
		_ = file.Close()
		return nil, nil, err
	}
	load()
	if n.polling {
		if err := n.arm(); err != nil {
			_ = file.Close()
			return nil, nil, err
		}
	}
	ctx, cancel := context.WithCancel(ctx)
	stopClose := context.AfterFunc(ctx, func() { _ = file.Close() })
	changes, done := make(chan error, 1), make(chan struct{})
	go func() {
		defer close(done)
		defer close(changes)
		defer file.Close()
		var buf [64 * 1024]byte
		for {
			var err error
			if n.polling {
				wait := time.NewTimer(time.Second)
				select {
				case <-ctx.Done():
					wait.Stop()
					return
				case <-wait.C:
				}
				err = n.arm()
				if err == nil && n.polling {
					// Still no journal: nothing to reload. Signalling only after the journal is watched means no
					// frame can show it while discovery still ticks.
					continue
				}
			} else {
				count, readErr := file.Read(buf[:])
				if ctx.Err() != nil {
					return
				}
				if readErr == nil && !n.relevant(buf[:count]) {
					continue
				}
				if readErr != nil {
					err = fmt.Errorf("read journal notifications: %w", readErr)
				} else {
					err = n.arm()
				}
			}
			select {
			case changes <- err:
			case <-ctx.Done():
				return
			}
			if err != nil {
				return
			}
		}
	}()
	stop := func() {
		cancel()
		<-done
		stopClose()
	}
	return changes, stop, nil
}
