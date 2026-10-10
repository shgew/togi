package machine

import (
	"bufio"
	"compress/gzip"
	"encoding/json"
	"errors"
	"io"
	"io/fs"
	"iter"
	"os"
	"path/filepath"
)

// SamplesFile names a trial's conditions samples; trial files are gzip-compressed to name+CompressedSuffix when their trial ends.
const (
	SamplesFile      = "samples.jsonl"
	CompressedSuffix = ".gz"
)

// ReadSamples yields the complete samples of the trial directory dir, from either form of its samples file.
func ReadSamples(dir string) iter.Seq[TrialConditions] {
	return func(yield func(TrialConditions) bool) {
		f, err := OpenTrialFile(filepath.Join(dir, SamplesFile))
		if err != nil {
			return
		}
		defer f.Close()
		reader := bufio.NewReader(f)
		for {
			line, err := reader.ReadBytes('\n')
			if err != nil {
				return
			}
			var sample TrialConditions
			if json.Unmarshal(line, &sample) == nil && !yield(sample) {
				return
			}
		}
	}
}

// OpenTrialFile opens the trial file at path, or its compressed form when the plain one is gone. Compression removes the
// plain file only after the compressed one is complete, so one of the two is always readable.
func OpenTrialFile(path string) (io.ReadCloser, error) {
	f, err := os.Open(path)
	if err == nil {
		return f, nil
	}
	if !errors.Is(err, fs.ErrNotExist) {
		return nil, err
	}
	f, err = os.Open(path + CompressedSuffix)
	if err != nil {
		return nil, err
	}
	z, err := gzip.NewReader(f)
	if err != nil {
		f.Close()
		return nil, err
	}
	return compressedFile{Reader: z, file: f}, nil
}

type compressedFile struct {
	*gzip.Reader
	file *os.File
}

func (c compressedFile) Close() error {
	return errors.Join(c.Reader.Close(), c.file.Close())
}
