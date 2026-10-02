package sim

import (
	"bufio"
	"encoding/json"
	"fmt"
	"iter"
	"os"
	"path/filepath"
	"slices"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func (m *Machine) SetSamplesDir(dir string) { m.samplesDir = dir }

type trialSamples struct {
	spec        machine.TrialSpec
	ran         time.Duration
	stalledCore int
}

func (s trialSamples) conditions() iter.Seq[machine.TrialConditions] {
	return func(yield func(machine.TrialConditions) bool) {
		stallAt := max(time.Second, s.ran-2*time.Second)
		for at := time.Second; at < s.ran; at += time.Second {
			p := machine.TrialConditions{ElapsedMS: at.Milliseconds(), WorkerCPUMS: make(map[int]int64, len(s.spec.Cores))}
			for _, core := range s.spec.Cores {
				cpu := at
				if core == s.stalledCore {
					cpu = min(cpu, stallAt)
				}
				p.WorkerCPUMS[core] = cpu.Milliseconds() * int64(s.spec.Workload.Threads)
			}
			if !yield(p) {
				return
			}
		}
	}
}

func (m *Machine) PMTable() *machine.PMTable { return nil }

func (t trials) Samples(id string) iter.Seq[machine.TrialConditions] {
	return func(yield func(machine.TrialConditions) bool) {
		if t.m.samplesDir == "" && id == t.m.samples.spec.ID {
			for sample := range t.m.samples.conditions() {
				if !yield(sample) {
					return
				}
			}
			return
		}
		if t.m.samplesDir == "" {
			return
		}
		f, err := os.Open(filepath.Join(t.m.samplesDir, id, "samples.jsonl"))
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
			var sample machine.TrialConditions
			if json.Unmarshal(line, &sample) == nil && !yield(sample) {
				return
			}
		}
	}
}

func (r *running) sampleConditions(ran time.Duration, stalledCore int) error {
	m := r.m
	samples := trialSamples{spec: r.spec, ran: ran, stalledCore: stalledCore}
	m.samples = trialSamples{}
	if m.samplesDir == "" {
		samples.spec.Cores = slices.Clone(samples.spec.Cores)
		m.samples = samples
		return nil
	}
	dir := filepath.Join(m.samplesDir, r.spec.ID)
	if err := os.MkdirAll(dir, 0711); err != nil {
		return fmt.Errorf("create simulated sample directory: %w", err)
	}
	f, err := os.Create(filepath.Join(dir, "samples.jsonl"))
	if err != nil {
		return fmt.Errorf("create simulated samples: %w", err)
	}
	encoder := json.NewEncoder(f)
	for p := range samples.conditions() {
		if err = encoder.Encode(p); err != nil {
			break
		}
	}
	closeErr := f.Close()
	if err == nil {
		err = closeErr
	}
	if err != nil {
		return fmt.Errorf("persist simulated samples: %w", err)
	}
	return nil
}
