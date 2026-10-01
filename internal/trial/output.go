package trial

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"syscall"
	"time"
)

const (
	outputLineLimit = 64 * 1024
	watchReadLimit  = 64 * 1024
)

var (
	errOutputLineTooLong = errors.New("backend output line exceeds 64 KiB")
	errWatchedOutputLost = errors.New("watched output lost before draining")
)

// contextError checks the clock as well as cancellation: a deadline timer may
// not yet have published Err when a cooperative polling boundary is reached.
func contextError(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if deadline, timed := ctx.Deadline(); timed && !time.Now().Before(deadline) {
		return context.DeadlineExceeded
	}
	return nil
}

type outputLines struct {
	pending  []byte
	exceeded bool
}

func (l *outputLines) consume(ctx context.Context, data []byte, line func(string)) (int, error) {
	used := 0
	deadline, timed := ctx.Deadline()
	for len(data) > 0 {
		if err := ctx.Err(); err != nil {
			return used, err
		}
		if timed && !time.Now().Before(deadline) {
			return used, context.DeadlineExceeded
		}
		delimiter := bytesIndexDelimiter(data)
		length := len(data)
		if delimiter >= 0 {
			length = delimiter
		}
		keep := min(length, outputLineLimit-len(l.pending))
		l.retain(data[:keep])
		used += keep
		if keep < length {
			l.exceeded = true
			return used, errOutputLineTooLong
		}
		if delimiter < 0 {
			return used, nil
		}
		line(string(l.pending))
		l.pending = l.pending[:0]
		used++
		data = data[delimiter+1:]
	}
	return used, nil
}

func (l *outputLines) retain(data []byte) {
	length := len(l.pending) + len(data)
	if length > cap(l.pending) {
		grown := make([]byte, len(l.pending), min(outputLineLimit, max(length, 2*cap(l.pending))))
		copy(grown, l.pending)
		l.pending = grown
	}
	l.pending = append(l.pending, data...)
}

func bytesIndexDelimiter(b []byte) int {
	for i, c := range b {
		if c == '\n' || c == '\r' {
			return i
		}
	}
	return -1
}

func (w *watchFile) read(ctx context.Context, line func(string)) (bool, error) {
	if err := ctx.Err(); err != nil {
		return false, err
	}
	deadline, timed := ctx.Deadline()
	if timed && !time.Now().Before(deadline) {
		return false, context.DeadlineExceeded
	}
	if w.lines.exceeded {
		return false, nil
	}
	f, err := os.OpenFile(w.path, os.O_RDONLY|syscall.O_NOFOLLOW|syscall.O_NONBLOCK, 0)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) && w.offset < w.size {
			err = fmt.Errorf("%w: %w", errWatchedOutputLost, err)
		}
		return false, err
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil {
		return false, fmt.Errorf("stat: %w", err)
	}
	if !info.Mode().IsRegular() {
		return false, fmt.Errorf("watched file is not regular: %s", w.path)
	}
	if info.Size() < w.size && w.offset < w.size {
		return false, fmt.Errorf("%w: file truncated from %d to %d bytes with %d unread", errWatchedOutputLost, w.size, info.Size(), w.size-w.offset)
	}
	w.size = info.Size()
	if _, err := f.Seek(w.offset, io.SeekStart); err != nil {
		return false, fmt.Errorf("seek: %w", err)
	}
	var buf [4096]byte
	remaining := info.Size() - w.offset
	budget := int64(watchReadLimit)
	for remaining > 0 && budget > 0 {
		if err := ctx.Err(); err != nil {
			return false, err
		}
		if timed && !time.Now().Before(deadline) {
			return false, context.DeadlineExceeded
		}
		n, err := f.Read(buf[:min(int64(len(buf)), remaining, budget)])
		used, lineErr := w.lines.consume(ctx, buf[:n], line)
		w.offset += int64(used)
		remaining -= int64(used)
		budget -= int64(used)
		if lineErr != nil {
			return false, lineErr
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				return remaining <= 0, nil
			}
			return false, err
		}
		if n == 0 {
			return false, io.ErrNoProgress
		}
	}
	if err := ctx.Err(); err != nil {
		return false, err
	}
	if timed && !time.Now().Before(deadline) {
		return false, context.DeadlineExceeded
	}
	return remaining <= 0, nil
}
