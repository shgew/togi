package sim

import (
	"bufio"
	"encoding/json"
	"fmt"
	"iter"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
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
		for at := time.Second; at < s.ran; at += time.Second {
			p := machine.TrialConditions{ElapsedMS: at.Milliseconds(), WorkerCPUMS: make(map[int]int64, len(s.spec.Cores))}
			for _, core := range s.spec.Cores {
				p.WorkerCPUMS[core] = s.workerCPUMS(core, at)
			}
			if !yield(p) {
				return
			}
		}
	}
}

func (s trialSamples) workerCPUMS(core int, at time.Duration) int64 {
	if core == s.stalledCore {
		at = min(at, max(time.Second, s.ran-2*time.Second))
	}
	return at.Milliseconds() * int64(s.spec.Workload.Threads)
}

// appendLines writes what json.Encoder writes for conditions: the map keys sort as strings.
func (s trialSamples) appendLines(w *bufio.Writer) error {
	cores := slices.Clone(s.spec.Cores)
	slices.SortFunc(cores, func(a, b int) int { return strings.Compare(strconv.Itoa(a), strconv.Itoa(b)) })
	cores = slices.Compact(cores)
	var b []byte
	for at := time.Second; at < s.ran; at += time.Second {
		b = append(b[:0], `{"elapsed_ms":`...)
		b = strconv.AppendInt(b, at.Milliseconds(), 10)
		if len(cores) > 0 {
			b = append(b, `,"worker_cpu_ms":{`...)
			for i, core := range cores {
				if i > 0 {
					b = append(b, ',')
				}
				b = append(b, '"')
				b = strconv.AppendInt(b, int64(core), 10)
				b = append(b, `":`...)
				b = strconv.AppendInt(b, s.workerCPUMS(core, at), 10)
			}
			b = append(b, '}')
		}
		b = append(b, "}\n"...)
		if _, err := w.Write(b); err != nil {
			return err
		}
	}
	return nil
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
	w := bufio.NewWriter(f)
	err = samples.appendLines(w)
	if err == nil {
		err = w.Flush()
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
