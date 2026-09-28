// Package backend is the contract between the trial runner and the stress programs it launches.
package backend

import "github.com/shgew/togi/internal/machine"

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
