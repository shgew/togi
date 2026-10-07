package forecast

import (
	"errors"
	"fmt"
	"math"
	"slices"
	"strconv"
)

// Range describes an ensemble's values: the median, the 10th to 90th percentile and the extremes. Percentiles use
// the nearest rank, so each is a value some run produced; the median of an even count averages the middle two.
type Range struct {
	Min    float64 `json:"min"`
	P10    float64 `json:"p10"`
	Median float64 `json:"median"`
	P90    float64 `json:"p90"`
	Max    float64 `json:"max"`
}

func rangeOf(sorted []float64) Range {
	n := len(sorted)
	rank := func(p float64) float64 { return sorted[max(int(math.Ceil(p*float64(n)/100)), 1)-1] }
	median := sorted[n/2]
	if n%2 == 0 {
		median = (sorted[n/2-1] + sorted[n/2]) / 2
	}
	return Range{Min: sorted[0], P10: rank(10), Median: median, P90: rank(90), Max: sorted[n-1]}
}

// Metric is one forecast quantity across the ensemble.
type Metric struct {
	Name string
	// Range is nil when no run supplies the metric: hours when no run concluded.
	Range  *Range
	hours  bool
	values []float64
}

// Format renders a value of the metric: hours to two decimals, counts and offsets as they are.
func (m Metric) Format(v float64) string {
	if m.hours {
		return strconv.FormatFloat(v, 'f', 2, 64)
	}
	return strconv.FormatFloat(v, 'f', -1, 64)
}

// Percentile is the share of runs below v plus half of those equal to it, in percent; with below, only the share
// below v, which bounds the percentile of a value that can still grow.
func (m Metric) Percentile(v float64, below bool) float64 {
	var less, equal int
	for _, x := range m.values {
		switch {
		case x < v:
			less++
		case x == v:
			equal++
		}
	}
	if below {
		equal = 0
	}
	return 100 * (float64(less) + float64(equal)/2) / float64(len(m.values))
}

// Metrics returns the forecast quantities in report order: each core's final offset, the total depth, hours to
// conclusion over concluded runs only, crashes and hunts. Every run counts in the others, so the crashes and hunts
// of runs that did not conclude enter as lower bounds.
func Metrics(runs []Run) ([]Metric, error) {
	if len(runs) == 0 {
		return nil, errors.New("forecast has no runs")
	}
	cores := len(runs[0].Profile)
	for _, r := range runs {
		if len(r.Profile) != cores {
			return nil, fmt.Errorf("run %s seed %d has %d cores, others %d", r.Machine, r.Seed, len(r.Profile), cores)
		}
	}
	metrics := make([]Metric, 0, cores+4)
	add := func(name string, hours bool, value func(Run) (float64, bool)) {
		m := Metric{Name: name, hours: hours}
		for _, r := range runs {
			if v, ok := value(r); ok {
				m.values = append(m.values, v)
			}
		}
		if len(m.values) > 0 {
			slices.Sort(m.values)
			m.Range = new(rangeOf(m.values))
		}
		metrics = append(metrics, m)
	}
	for core := range cores {
		add(fmt.Sprintf("core %02d", core), false, func(r Run) (float64, bool) { return float64(r.Profile[core]), true })
	}
	add("depth", false, func(r Run) (float64, bool) { return float64(r.Depth), true })
	add("hours", true, func(r Run) (float64, bool) { return r.Hours, r.Status == Concluded })
	add("crashes", false, func(r Run) (float64, bool) { return float64(r.Crashes), true })
	add("hunts", false, func(r Run) (float64, bool) { return float64(r.Hunts), true })
	return metrics, nil
}

// Summary is the ensemble's statuses and ranges, as Metrics computes them.
type Summary struct {
	Runs      int `json:"runs"`
	Concluded int `json:"concluded"`
	DeadEnds  int `json:"dead_ends"`
	Censored  int `json:"censored"`
	// Hours covers concluded runs only; it is null when none concluded.
	Hours   *Range  `json:"hours"`
	Crashes Range   `json:"crashes"`
	Hunts   Range   `json:"hunts"`
	Depth   Range   `json:"depth"`
	Cores   []Range `json:"cores"`
}

// Summarize counts the runs by status and ranges each metric.
func Summarize(runs []Run) (Summary, error) {
	metrics, err := Metrics(runs)
	if err != nil {
		return Summary{}, err
	}
	s := Summary{Runs: len(runs)}
	for _, r := range runs {
		switch r.Status {
		case Concluded:
			s.Concluded++
		case DeadEnd:
			s.DeadEnds++
		case Censored:
			s.Censored++
		default:
			return Summary{}, fmt.Errorf("run %s seed %d has status %q", r.Machine, r.Seed, r.Status)
		}
	}
	cores := len(metrics) - 4
	for _, m := range metrics[:cores] {
		s.Cores = append(s.Cores, *m.Range)
	}
	s.Depth, s.Hours, s.Crashes, s.Hunts = *metrics[cores].Range, metrics[cores+1].Range, *metrics[cores+2].Range, *metrics[cores+3].Range
	return s, nil
}
