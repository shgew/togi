package session

import (
	"iter"
	"slices"

	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/requests"
)

type sampleSummary struct {
	last            *machine.TrialConditions
	stalledCore     *int
	workerStalledMS *int64
	voltageMedianV  *float64
	voltageMinV     *float64
	requests        requests.Telemetry
}

// sampleEvidence maps loaded cores to CCDs through ccds, the session's recorded
// topology, and omits request telemetry when a loaded core is missing from it.
func sampleEvidence(samples iter.Seq[machine.TrialConditions], cores []int, regime machine.Regime, ccds map[int]int) sampleSummary {
	type worker struct {
		cpu   int64
		stall *int64
	}
	workers := make([]worker, len(cores))
	var last *machine.TrialConditions
	valid := len(cores) >= 2 && regime != machine.R6
	count := 0
	var voltage requestedVoltage
	observed := func(yield func(machine.TrialConditions) bool) {
		more := true
		for sample := range samples {
			voltage.add(sample, cores)
			for i, core := range cores {
				cpu, present := sample.WorkerCPUMS[core]
				if !present || cpu < 0 || count > 0 && (cpu < workers[i].cpu || sample.ElapsedMS <= last.ElapsedMS) {
					valid = false
				}
				if count > 0 && cpu == workers[i].cpu {
					if workers[i].stall == nil {
						workers[i].stall = new(sample.ElapsedMS)
					}
				} else {
					workers[i].stall = nil
				}
				workers[i].cpu = cpu
			}
			last = &sample
			count++
			if more {
				more = yield(sample)
			}
		}
	}
	var telemetry requests.Telemetry
	if len(cores) == 0 || slices.ContainsFunc(cores, func(core int) bool { _, ok := ccds[core]; return !ok }) {
		observed(func(machine.TrialConditions) bool { return true })
	} else {
		telemetry, _ = requests.Summarize(observed, cores, func(core int) int { return ccds[core] })
	}
	median, minimum := voltage.medianMinimum()
	summary := sampleSummary{last: last, voltageMedianV: median, voltageMinV: minimum, requests: telemetry}
	if !valid || count < 2 {
		return summary
	}
	first := -1
	tie := false
	for i, w := range workers {
		if w.stall == nil {
			continue
		}
		if first < 0 || *w.stall < *workers[first].stall {
			first, tie = i, false
		} else if *w.stall == *workers[first].stall {
			tie = true
		}
	}
	if first < 0 || tie {
		return summary
	}
	summary.stalledCore, summary.workerStalledMS = new(cores[first]), workers[first].stall
	return summary
}

type requestedVoltage struct {
	maxima []float64
}

func (v *requestedVoltage) add(sample machine.TrialConditions, cores []int) {
	if sample.PMTable == nil || len(cores) == 0 {
		return
	}
	requests := &sample.PMTable.VoltageRequestV
	var highest float32
	for i, core := range cores {
		if core < 0 || core >= len(requests) {
			return
		}
		if i == 0 || requests[core] > highest {
			highest = requests[core]
		}
	}
	v.maxima = append(v.maxima, float64(highest))
}

func (v *requestedVoltage) medianMinimum() (*float64, *float64) {
	if len(v.maxima) == 0 {
		return nil, nil
	}
	slices.Sort(v.maxima)
	middle := len(v.maxima) / 2
	median := v.maxima[middle]
	if len(v.maxima)%2 == 0 {
		median = (v.maxima[middle-1] + median) / 2
	}
	return new(median), new(v.maxima[0])
}
