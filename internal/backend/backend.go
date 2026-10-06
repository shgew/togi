// Package backend is the contract between the trial runner and the stress programs it launches.
package backend

import (
	"errors"
	"fmt"
	"io/fs"
	"os"

	"github.com/shgew/togi/internal/machine"
)

type Kind int

const (
	Other Kind = iota
	Progress
	ComputationError
	SetupError
	AffinityError
)

type Line struct {
	Kind   Kind
	Detail string
	// CPU is set only for AffinityError.
	CPU int
}

type Launch struct {
	Argv  []string
	Files []string
	// Watch lists files in the work directory to tail for errors.
	Watch []string
}

type Backend interface {
	// Name is "mprime" or "y-cruncher".
	Name() string
	Check() (detail string, err error)
	Prepare(w machine.Workload, dir string, cpus []int) (Launch, error)
	Classify(line string) Line
}

// CheckExecutable checks that the named backend's binary at path is an executable regular file.
// A binary that does not exist wraps machine.ErrBackendMissing.
func CheckExecutable(name, path string) error {
	info, err := os.Stat(path)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return fmt.Errorf("stat %s binary %s: %w: %w", name, path, machine.ErrBackendMissing, err)
		}
		return fmt.Errorf("stat %s binary %s: %w", name, path, err)
	}
	if !info.Mode().IsRegular() || info.Mode().Perm()&0111 == 0 {
		return fmt.Errorf("%s binary %s is not executable", name, path)
	}
	return nil
}
