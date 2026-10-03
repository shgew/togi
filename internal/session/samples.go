package session

import (
	"iter"

	"github.com/shgew/togi/internal/machine"
)

type sampleSummary struct {
	last            *machine.TrialConditions
	stalledCore     *int
	workerStalledMS *int64
}

func sampleEvidence(samples iter.Seq[machine.TrialConditions], cores []int, regime machine.Regime) sampleSummary {
	type worker struct {
		cpu   int64
		stall *int64
	}
	workers := make([]worker, len(cores))
	var last *machine.TrialConditions
	valid := len(cores) >= 2 && regime != machine.R6
	count := 0
	for sample := range samples {
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
	}
	if !valid || count < 2 {
		return sampleSummary{last: last}
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
		return sampleSummary{last: last}
	}
	return sampleSummary{last: last, stalledCore: new(cores[first]), workerStalledMS: workers[first].stall}
}
