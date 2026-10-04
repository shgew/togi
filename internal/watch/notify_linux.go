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

	"golang.org/x/sys/unix"
)

const journalWatchMask = unix.IN_MODIFY | unix.IN_CLOSE_WRITE | unix.IN_ATTRIB | unix.IN_CREATE | unix.IN_MOVED_FROM | unix.IN_MOVED_TO | unix.IN_DELETE | unix.IN_DELETE_SELF | unix.IN_MOVE_SELF | unix.IN_ONLYDIR

type journalNotifier struct {
	fd      int
	dir     string
	watches map[int]string
}

func (n *journalNotifier) arm() error {
	watches := make(map[int]string, 2)
	path := n.dir
	for {
		wd, err := unix.InotifyAddWatch(n.fd, path, journalWatchMask)
		if err == nil {
			watches[wd] = path
			if path != n.dir || path == filepath.Dir(path) {
				break
			}
		} else if !errors.Is(err, unix.ENOENT) && !errors.Is(err, unix.ENOTDIR) {
			return fmt.Errorf("watch state directory: %w", err)
		}
		parent := filepath.Dir(path)
		if parent == path {
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
		if mask&(unix.IN_IGNORED|unix.IN_DELETE_SELF|unix.IN_MOVE_SELF) != 0 {
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

func watchJournal(ctx context.Context, dir string) (<-chan error, func(), error) {
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
	ctx, cancel := context.WithCancel(ctx)
	stopClose := context.AfterFunc(ctx, func() { _ = file.Close() })
	changes, done := make(chan error, 1), make(chan struct{})
	go func() {
		defer close(done)
		defer close(changes)
		defer file.Close()
		var buf [64 * 1024]byte
		for {
			count, err := file.Read(buf[:])
			if ctx.Err() != nil {
				return
			}
			if err == nil && !n.relevant(buf[:count]) {
				continue
			}
			if err != nil {
				err = fmt.Errorf("read journal notifications: %w", err)
			} else {
				err = n.arm()
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
