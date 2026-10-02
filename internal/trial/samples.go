package trial

import (
	"bufio"
	"encoding/json"
	"fmt"
	"iter"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/shgew/togi/internal/machine"
)

type conditionsSampler struct {
	options  Options
	cpus     map[int]string
	energy   *int64
	energyAt time.Time
}

func newConditionsSampler(o Options, spec machine.TrialSpec, started time.Time) *conditionsSampler {
	s := &conditionsSampler{options: o, cpus: make(map[int]string), energyAt: started}
	for _, core := range spec.Cores {
		for _, info := range o.Cores {
			if info.Core == core && len(info.CPUs) > 0 {
				s.cpus[core] = filepath.Join(o.CPUFreq, fmt.Sprintf("cpu%d", info.CPUs[0]), "cpufreq", "scaling_cur_freq")
				break
			}
		}
	}
	s.energy = readSensor(filepath.Join(o.Powercap, "intel-rapl:0", "energy_uj"))
	return s
}

func (s *conditionsSampler) sample(started time.Time) machine.TrialConditions {
	p := machine.TrialConditions{CoreMHz: make(map[int]int)}
	p.TctlC, p.TccdC = readTemperatures(s.options.Hwmon)
	for core, path := range s.cpus {
		if value := readSensor(path); value != nil && *value >= 0 {
			p.CoreMHz[core] = int(*value / 1000)
		}
	}
	energy := readSensor(filepath.Join(s.options.Powercap, "intel-rapl:0", "energy_uj"))
	now := time.Now()
	p.ElapsedMS = now.Sub(started).Milliseconds()
	if energy != nil && s.energy != nil && now.After(s.energyAt) {
		delta := *energy - *s.energy
		if delta < 0 {
			if limit := readSensor(filepath.Join(s.options.Powercap, "intel-rapl:0", "max_energy_range_uj")); limit != nil && *limit > *s.energy {
				delta += *limit
			}
		}
		if delta >= 0 {
			watts := float64(delta) / 1e6 / now.Sub(s.energyAt).Seconds()
			p.PackagePowerW = &watts
		}
	}
	s.energy, s.energyAt = energy, now
	return p
}

func readSensor(path string) *int64 {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	value, err := strconv.ParseInt(strings.TrimSpace(string(b)), 10, 64)
	if err != nil {
		return nil
	}
	return &value
}

func readTemperatures(root string) (*int, map[string]int) {
	dirs, err := os.ReadDir(root)
	if err != nil {
		return nil, nil
	}
	for _, d := range dirs {
		dir := filepath.Join(root, d.Name())
		name, err := os.ReadFile(filepath.Join(dir, "name"))
		if err != nil || (strings.TrimSpace(string(name)) != "k10temp" && strings.TrimSpace(string(name)) != "zenpower") {
			continue
		}
		var tctl *int
		tccd := make(map[string]int)
		labels, _ := filepath.Glob(filepath.Join(dir, "temp*_label"))
		for _, path := range labels {
			b, err := os.ReadFile(path)
			if err != nil {
				continue
			}
			label := strings.TrimSpace(string(b))
			ccd, ccdErr := strconv.Atoi(strings.TrimPrefix(label, "Tccd"))
			if label != "Tctl" && (!strings.HasPrefix(label, "Tccd") || ccdErr != nil || ccd < 1) {
				continue
			}
			value := readSensor(strings.TrimSuffix(path, "_label") + "_input")
			if value == nil {
				continue
			}
			degrees := int(*value / 1000)
			if label == "Tctl" {
				tctl = &degrees
			} else {
				tccd[label] = degrees
			}
		}
		return tctl, tccd
	}
	return nil, nil
}

type sampleFile interface {
	Write([]byte) (int, error)
	Sync() error
	Close() error
}

func openSamples(dir string) (sampleFile, error) {
	f, err := os.OpenFile(filepath.Join(dir, "samples.jsonl"), os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0644)
	if err != nil {
		return nil, fmt.Errorf("create trial samples: %w", err)
	}
	for _, path := range []string{dir, filepath.Dir(dir)} {
		d, err := os.Open(path)
		if err == nil {
			err = d.Sync()
			closeErr := d.Close()
			if err == nil {
				err = closeErr
			}
		}
		if err != nil {
			_ = f.Close()
			return nil, fmt.Errorf("sync trial samples directory: %w", err)
		}
	}
	return f, nil
}

func appendSample(f sampleFile, p machine.TrialConditions) error {
	line, err := json.Marshal(p)
	if err == nil {
		_, err = f.Write(append(line, '\n'))
	}
	if err == nil {
		err = f.Sync()
	}
	if err != nil {
		return fmt.Errorf("persist trial sample: %w", err)
	}
	return nil
}

func (r *Runner) Samples(id string) iter.Seq[machine.TrialConditions] {
	return func(yield func(machine.TrialConditions) bool) {
		f, err := os.Open(filepath.Join(r.options.Dir, id, "samples.jsonl"))
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
			var p machine.TrialConditions
			if json.Unmarshal(line, &p) == nil && !yield(p) {
				return
			}
		}
	}
}
