// PROTOTYPE, throwaway: Go side of the Go-vs-Rust kernel benchmark.
// "verbatim" copies internal/sim/samples.go conditions, internal/session/samples.go sampleEvidence and
// internal/requests Summarize as they are on main; "tuned" is the same logic without per-sample allocation.
package main

import (
	"bufio"
	"cmp"
	"flag"
	"fmt"
	"iter"
	"maps"
	"math"
	"os"
	"slices"
	"strconv"
	"strings"
	"time"
)

type PMTable struct {
	PowerW, VoltageRequestV, TemperatureC, C0Pct, CC1Pct, CC6Pct [16]float32
}

type TrialConditions struct {
	ElapsedMS     int64
	TctlC         *int
	TccdC         map[string]int
	CoreMHz       map[int]int
	WorkerCPUMS   map[int]int64
	PackagePowerW *float64
	PMTable       *PMTable
}

type trial struct {
	ranS, stalled, threads int
	cores                  []int
}

const (
	warmupMS   = 5000
	minSamples = 20
	tieV       = 0.001
)

func requestFor(core int) float32 { return 1.1 + 0.002*float32(core%8) + 0.0005*float32(core/8) }
func clockFor(ccd int) int        { return 5000 + 100*ccd }

// ---- verbatim ----

type trialSamples struct {
	cores       []int
	threads     int
	ran         time.Duration
	stalledCore int
	requests    [16]float32
	clocks      [2]int
}

func (s trialSamples) conditions() iter.Seq[TrialConditions] {
	return func(yield func(TrialConditions) bool) {
		pm := &PMTable{VoltageRequestV: s.requests}
		mhz := make(map[int]int, len(s.cores))
		for _, core := range s.cores {
			mhz[core] = s.clocks[core/8]
			pm.C0Pct[core] = 100
		}
		for core := range 16 {
			pm.CC6Pct[core] = 100 - pm.C0Pct[core]
		}
		for at := time.Second; at < s.ran; at += time.Second {
			cpu := make(map[int]int64, len(s.cores))
			for _, core := range s.cores {
				cpu[core] = s.workerCPUMS(core, at)
			}
			if !yield(TrialConditions{ElapsedMS: at.Milliseconds(), WorkerCPUMS: cpu, PMTable: pm, CoreMHz: mhz}) {
				return
			}
		}
	}
}

func (s trialSamples) workerCPUMS(core int, at time.Duration) int64 {
	if core == s.stalledCore {
		at = min(at, max(time.Second, s.ran-2*time.Second))
	}
	return at.Milliseconds() * int64(s.threads)
}

type Telemetry struct {
	Requests      map[int]float64
	TopRequesters []int
	CCDMHz        map[int]int
}

type sampleSummary struct {
	last            *TrialConditions
	stalledCore     *int
	workerStalledMS *int64
	voltageMedianV  *float64
	voltageMinV     *float64
	requests        Telemetry
}

func sampleEvidence(samples iter.Seq[TrialConditions], cores []int, ccds map[int]int) sampleSummary {
	type worker struct {
		cpu   int64
		stall *int64
	}
	workers := make([]worker, len(cores))
	var last *TrialConditions
	valid := len(cores) >= 2
	count := 0
	var voltage requestedVoltage
	observed := func(yield func(TrialConditions) bool) {
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
	var telemetry Telemetry
	if len(cores) == 0 || slices.ContainsFunc(cores, func(core int) bool { _, ok := ccds[core]; return !ok }) {
		observed(func(TrialConditions) bool { return true })
	} else {
		telemetry, _ = summarize(observed, cores, func(core int) int { return ccds[core] })
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

type requestedVoltage struct{ maxima []float64 }

func (v *requestedVoltage) add(sample TrialConditions, cores []int) {
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

func summarize(samples iter.Seq[TrialConditions], cores []int, ccdOf func(core int) int) (Telemetry, bool) {
	if len(cores) == 0 {
		return Telemetry{}, false
	}
	requests := make(map[int][]float64, len(cores))
	clocks := map[int][]float64{}
	perCCD := map[int][]float64{}
	count := 0
	for sample := range samples {
		if sample.ElapsedMS < warmupMS || sample.PMTable == nil {
			continue
		}
		lanes := &sample.PMTable.VoltageRequestV
		if slices.ContainsFunc(cores, func(core int) bool { return core < 0 || core >= len(lanes) }) {
			return Telemetry{}, false
		}
		for ccd, mhz := range perCCD {
			perCCD[ccd] = mhz[:0]
		}
		for _, core := range cores {
			requests[core] = append(requests[core], float64(lanes[core]))
			if mhz, ok := sample.CoreMHz[core]; ok {
				perCCD[ccdOf(core)] = append(perCCD[ccdOf(core)], float64(mhz))
			}
		}
		for ccd, mhz := range perCCD {
			if len(mhz) > 0 {
				clocks[ccd] = append(clocks[ccd], medianInPlace(mhz))
			}
		}
		count++
	}
	if count < minSamples {
		return Telemetry{}, false
	}
	t := Telemetry{Requests: make(map[int]float64, len(cores))}
	byCCD := map[int]map[int]float64{}
	for core, values := range requests {
		t.Requests[core] = median(values)
		ccd := ccdOf(core)
		if byCCD[ccd] == nil {
			byCCD[ccd] = map[int]float64{}
		}
		byCCD[ccd][core] = t.Requests[core]
	}
	for _, group := range byCCD {
		t.TopRequesters = append(t.TopRequesters, groups(group)[0]...)
	}
	slices.Sort(t.TopRequesters)
	for ccd, mhz := range clocks {
		if len(mhz) < minSamples {
			continue
		}
		if t.CCDMHz == nil {
			t.CCDMHz = map[int]int{}
		}
		t.CCDMHz[ccd] = int(math.Round(median(mhz)))
	}
	return t, true
}

func groups(requests map[int]float64) [][]int {
	cores := slices.SortedFunc(maps.Keys(requests), func(a, b int) int {
		if c := cmp.Compare(requests[b], requests[a]); c != 0 {
			return c
		}
		return cmp.Compare(a, b)
	})
	var groups [][]int
	for len(cores) > 0 {
		n := 1
		for n < len(cores) && requests[cores[n]] >= requests[cores[0]]-tieV {
			n++
		}
		group := slices.Clone(cores[:n])
		slices.Sort(group)
		groups = append(groups, group)
		cores = cores[n:]
	}
	return groups
}

func median(values []float64) float64 { return medianInPlace(slices.Clone(values)) }

func medianInPlace(values []float64) float64 {
	slices.Sort(values)
	middle := len(values) / 2
	if len(values)%2 == 0 {
		return (values[middle-1] + values[middle]) / 2
	}
	return values[middle]
}

func runVerbatim(t trial) string {
	s := trialSamples{cores: t.cores, threads: t.threads, ran: time.Duration(t.ranS) * time.Second, stalledCore: t.stalled}
	ccds := make(map[int]int, len(t.cores))
	for core := range 16 {
		s.requests[core] = requestFor(core)
		ccds[core] = core / 8
	}
	s.clocks = [2]int{clockFor(0), clockFor(1)}
	sum := sampleEvidence(s.conditions(), t.cores, ccds)
	var last int64 = -1
	if sum.last != nil {
		last = sum.last.ElapsedMS
	}
	var o out
	o.last = last
	o.stalled, o.stallMS = -1, -1
	if sum.stalledCore != nil {
		o.stalled, o.stallMS = *sum.stalledCore, *sum.workerStalledMS
	}
	if sum.voltageMedianV != nil {
		o.hasV, o.medV, o.minV = true, *sum.voltageMedianV, *sum.voltageMinV
	}
	for _, c := range slices.Sorted(maps.Keys(sum.requests.Requests)) {
		o.req = append(o.req, kv{c, sum.requests.Requests[c]})
	}
	o.top = sum.requests.TopRequesters
	for _, c := range slices.Sorted(maps.Keys(sum.requests.CCDMHz)) {
		o.mhz = append(o.mhz, [2]int{c, sum.requests.CCDMHz[c]})
	}
	return o.String()
}

// ---- tuned: same results, no per-sample heap allocation ----

type tunedSample struct {
	elapsedMS int64
	cpu       [16]int64
	present   uint16
	mhz       *[16]int32
	mhzSet    uint16
	pm        *PMTable
}

type tunedScratch struct {
	maxima   []float64
	requests [16][]float64
	clocks   [2][]float64
	perCCD   [2][]float64
	tmp      []float64
}

func (sc *tunedScratch) reset() {
	sc.maxima = sc.maxima[:0]
	for i := range sc.requests {
		sc.requests[i] = sc.requests[i][:0]
	}
	for i := range sc.clocks {
		sc.clocks[i] = sc.clocks[i][:0]
	}
}

func (sc *tunedScratch) medianOf(values []float64) float64 {
	sc.tmp = append(sc.tmp[:0], values...)
	return medianInPlace(sc.tmp)
}

func runTuned(t trial, sc *tunedScratch) string {
	sc.reset()
	ran := time.Duration(t.ranS) * time.Second
	var pm PMTable
	var mhz [16]int32
	var mhzSet uint16
	for core := range 16 {
		pm.VoltageRequestV[core] = requestFor(core)
	}
	for _, core := range t.cores {
		mhz[core] = int32(clockFor(core / 8))
		mhzSet |= 1 << core
		pm.C0Pct[core] = 100
	}
	for core := range 16 {
		pm.CC6Pct[core] = 100 - pm.C0Pct[core]
	}
	type worker struct {
		cpu      int64
		stall    int64
		hasStall bool
	}
	var workers [16]worker
	valid := len(t.cores) >= 2
	count := 0
	var last tunedSample
	telemetryOK := len(t.cores) > 0
	reqCount := 0
	for at := time.Second; at < ran; at += time.Second {
		s := tunedSample{elapsedMS: at.Milliseconds(), mhz: &mhz, mhzSet: mhzSet, pm: &pm}
		for _, core := range t.cores {
			a := at
			if core == t.stalled {
				a = min(a, max(time.Second, ran-2*time.Second))
			}
			s.cpu[core] = a.Milliseconds() * int64(t.threads)
			s.present |= 1 << core
		}
		if len(t.cores) > 0 {
			var highest float32
			for i, core := range t.cores {
				if i == 0 || s.pm.VoltageRequestV[core] > highest {
					highest = s.pm.VoltageRequestV[core]
				}
			}
			sc.maxima = append(sc.maxima, float64(highest))
		}
		for i, core := range t.cores {
			cpu, present := s.cpu[core], s.present&(1<<core) != 0
			if !present || cpu < 0 || count > 0 && (cpu < workers[i].cpu || s.elapsedMS <= last.elapsedMS) {
				valid = false
			}
			if count > 0 && cpu == workers[i].cpu {
				if !workers[i].hasStall {
					workers[i].stall, workers[i].hasStall = s.elapsedMS, true
				}
			} else {
				workers[i].hasStall = false
			}
			workers[i].cpu = cpu
		}
		last = s
		count++
		if telemetryOK && s.elapsedMS >= warmupMS {
			sc.perCCD[0], sc.perCCD[1] = sc.perCCD[0][:0], sc.perCCD[1][:0]
			for _, core := range t.cores {
				sc.requests[core] = append(sc.requests[core], float64(s.pm.VoltageRequestV[core]))
				if s.mhzSet&(1<<core) != 0 {
					sc.perCCD[core/8] = append(sc.perCCD[core/8], float64(s.mhz[core]))
				}
			}
			for ccd := range sc.perCCD {
				if len(sc.perCCD[ccd]) > 0 {
					sc.clocks[ccd] = append(sc.clocks[ccd], medianInPlace(sc.perCCD[ccd]))
				}
			}
			reqCount++
		}
	}
	var o out
	o.last, o.stalled, o.stallMS = -1, -1, -1
	if count > 0 {
		o.last = last.elapsedMS
	}
	if len(sc.maxima) > 0 {
		slices.Sort(sc.maxima)
		middle := len(sc.maxima) / 2
		med := sc.maxima[middle]
		if len(sc.maxima)%2 == 0 {
			med = (sc.maxima[middle-1] + med) / 2
		}
		o.hasV, o.medV, o.minV = true, med, sc.maxima[0]
	}
	if telemetryOK && reqCount >= minSamples {
		var reqs [16]float64
		var has uint16
		for core := range 16 {
			if len(sc.requests[core]) > 0 {
				reqs[core], has = sc.medianOf(sc.requests[core]), has|1<<core
				o.req = append(o.req, kv{core, reqs[core]})
			}
		}
		for ccd := range 2 {
			best, n := -1, 0
			var group [8]int
			for core := ccd * 8; core < ccd*8+8; core++ {
				if has&(1<<core) == 0 {
					continue
				}
				if best < 0 || reqs[core] > reqs[best] {
					best = core
				}
			}
			if best < 0 {
				continue
			}
			for core := ccd * 8; core < ccd*8+8; core++ {
				if has&(1<<core) != 0 && reqs[core] >= reqs[best]-tieV {
					group[n] = core
					n++
				}
			}
			o.top = append(o.top, group[:n]...)
		}
		slices.Sort(o.top)
		for ccd := range 2 {
			if len(sc.clocks[ccd]) >= minSamples {
				o.mhz = append(o.mhz, [2]int{ccd, int(math.Round(sc.medianOf(sc.clocks[ccd])))})
			}
		}
	}
	if valid && count >= 2 {
		first, tie := -1, false
		for i := range t.cores {
			w := workers[i]
			if !w.hasStall {
				continue
			}
			if first < 0 || w.stall < workers[first].stall {
				first, tie = i, false
			} else if w.stall == workers[first].stall {
				tie = true
			}
		}
		if first >= 0 && !tie {
			o.stalled, o.stallMS = t.cores[first], workers[first].stall
		}
	}
	return o.String()
}

// ---- shared harness ----

type kv struct {
	core int
	v    float64
}

type out struct {
	last          int64
	stalled       int
	stallMS       int64
	hasV          bool
	medV, minV    float64
	req           []kv
	top           []int
	mhz           [][2]int
}

func (o out) String() string {
	var b strings.Builder
	fmt.Fprintf(&b, "last=%d stall=%d@%d", o.last, o.stalled, o.stallMS)
	if o.hasV {
		fmt.Fprintf(&b, " v=%x/%x", math.Float64bits(o.medV), math.Float64bits(o.minV))
	}
	b.WriteString(" req=")
	for _, r := range o.req {
		fmt.Fprintf(&b, "%d:%x,", r.core, math.Float64bits(r.v))
	}
	b.WriteString(" top=")
	for _, c := range o.top {
		fmt.Fprintf(&b, "%d,", c)
	}
	b.WriteString(" mhz=")
	for _, m := range o.mhz {
		fmt.Fprintf(&b, "%d:%d,", m[0], m[1])
	}
	return b.String()
}

func load(path string) []trial {
	f, err := os.Open(path)
	if err != nil {
		panic(err)
	}
	defer f.Close()
	var ts []trial
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		p := strings.Split(sc.Text(), "\t")
		var t trial
		t.ranS, _ = strconv.Atoi(p[0])
		t.stalled, _ = strconv.Atoi(p[1])
		t.threads, _ = strconv.Atoi(p[2])
		for _, c := range strings.Split(p[3], ",") {
			n, _ := strconv.Atoi(c)
			t.cores = append(t.cores, n)
		}
		ts = append(ts, t)
	}
	return ts
}

func main() {
	variant := flag.String("variant", "verbatim", "verbatim or tuned")
	input := flag.String("in", "trials.tsv", "trial list")
	reps := flag.Int("reps", 1, "passes over the trial list")
	output := flag.String("out", "", "write per-trial results of the first pass here")
	flag.Parse()
	ts := load(*input)
	var w *bufio.Writer
	if *output != "" {
		f, _ := os.Create(*output)
		defer f.Close()
		w = bufio.NewWriter(f)
		defer w.Flush()
	}
	var sc tunedScratch
	start := time.Now()
	var h uint64 = 14695981039346656037
	for rep := range *reps {
		for _, t := range ts {
			var line string
			if *variant == "tuned" {
				line = runTuned(t, &sc)
			} else {
				line = runVerbatim(t)
			}
			for i := range len(line) {
				h = (h ^ uint64(line[i])) * 1099511628211
			}
			if rep == 0 && w != nil {
				w.WriteString(line)
				w.WriteByte('\n')
			}
		}
	}
	fmt.Fprintf(os.Stderr, "go-%s trials=%d reps=%d kernel=%.3fs hash=%016x\n", *variant, len(ts), *reps, time.Since(start).Seconds(), h)
}
