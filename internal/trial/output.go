package trial

import (
	"errors"
	"fmt"
	"io"
	"os"
)

const outputLineLimit = 64 * 1024

var errOutputLineTooLong = errors.New("backend output line exceeds 64 KiB")

type outputLines struct {
	pending  []byte
	exceeded bool
}

func (l *outputLines) consume(data []byte, line func(string)) (int, error) {
	used := 0
	for len(data) > 0 {
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

func (w *watchFile) read(line func(string)) error {
	if w.lines.exceeded {
		return nil
	}
	f, err := os.Open(w.path)
	if err != nil {
		return err
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil {
		return fmt.Errorf("stat: %w", err)
	}
	if _, err := f.Seek(w.offset, io.SeekStart); err != nil {
		return fmt.Errorf("seek: %w", err)
	}
	var buf [4096]byte
	remaining := info.Size() - w.offset
	for remaining > 0 {
		n, err := f.Read(buf[:min(int64(len(buf)), remaining)])
		w.offset += int64(n)
		remaining -= int64(n)
		if _, lineErr := w.lines.consume(buf[:n], line); lineErr != nil {
			return lineErr
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				return nil
			}
			return err
		}
	}
	return nil
}
