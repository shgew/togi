// Command exp493 turns bench outputs into the #493 comparison. It is a throwaway research program.
//
//	exp493 --reference r10 r10=r10.jsonl:KEEPDIR variantA=a.jsonl:KEEPDIR2 ...
package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/trialfacts"
)

var checkpoints = [...]float64{24, 48, 72, 120}

type record struct {
	Scenario          string   `json:"scenario"`
	Machine           string   `json:"machine"`
	Seed              uint64   `json:"seed"`
	Split             string   `json:"split"`
	Status            string   `json:"status"`
	SimHours          float64  `json:"sim_hours"`
	FirstPassedCycleH *float64 `json:"first_passed_cycle_h"`
	Crashes           int      `json:"crashes"`
	Hunts             int      `json:"hunts"`
	Depth             int      `json:"depth"`
	FinalProfile      []int    `json:"final_profile"`
	WorstR7HazardPerH *float64 `json:"worst_r7_hazard_per_h"`
}

type runKey struct {
	scenario, split string
	seed            uint64
}

type atCheckpoint struct {
	have   bool
	depth  int
	hazard *float64
}

type run struct {
	record
	journal bool
	confirm [len(checkpoints)]atCheckpoint
}

type variant struct {
	name string
	runs map[runKey]*run
	list []*run
}

type input struct {
	name, out, keep string
}

type validation struct {
	runs, withJournal                              int
	profile, hazard, simHours, crashes, hazardNone int
}

func main() { os.Exit(realMain(os.Args[1:], os.Stdout, os.Stderr)) }

func realMain(args []string, stdout, stderr io.Writer) int {
	fs := flag.NewFlagSet("exp493", flag.ContinueOnError)
	fs.SetOutput(stderr)
	reference := fs.String("reference", "", "name of the ruleset-10 variant")
	root := fs.String("root", ".", "worktree root; machine paths resolve against it and its tools/bench")
	jobs := fs.Int("jobs", 2, "journals analysed concurrently")
	fs.Usage = func() {
		fmt.Fprintln(stderr, "usage: exp493 --reference NAME NAME=OUT.jsonl:KEEPDIR [NAME=OUT.jsonl:KEEPDIR ...]")
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return 2
	}
	inputs, err := parseInputs(fs.Args())
	if err == nil && !slices.ContainsFunc(inputs, func(i input) bool { return i.name == *reference }) {
		err = fmt.Errorf("reference %q is not among the variants", *reference)
	}
	if err != nil {
		fmt.Fprintf(stderr, "exp493: %v\n", err)
		fs.Usage()
		return 2
	}
	machines := newMachines(*root)
	var variants []*variant
	var valid []validation
	for _, in := range inputs {
		v, val, err := load(in, machines, *jobs)
		if err != nil {
			fmt.Fprintf(stderr, "exp493: %s: %v\n", in.name, err)
			return 1
		}
		variants = append(variants, v)
		valid = append(valid, val)
	}
	slices.SortStableFunc(variants, func(a, b *variant) int { return boolInt(b.name == *reference) - boolInt(a.name == *reference) })
	report(stdout, variants, *reference, inputs, valid)
	return 0
}

func boolInt(b bool) int {
	if b {
		return 1
	}
	return 0
}

func parseInputs(args []string) ([]input, error) {
	var inputs []input
	seen := make(map[string]bool)
	for _, a := range args {
		name, rest, ok := strings.Cut(a, "=")
		cut := strings.LastIndex(rest, ":")
		if !ok || name == "" || cut < 1 || cut == len(rest)-1 {
			return nil, fmt.Errorf("argument %q is not NAME=OUT.jsonl:KEEPDIR", a)
		}
		if seen[name] {
			return nil, fmt.Errorf("variant %q repeats", name)
		}
		seen[name] = true
		inputs = append(inputs, input{name, expand(rest[:cut]), expand(rest[cut+1:])})
	}
	if len(inputs) == 0 {
		return nil, errors.New("no variants given")
	}
	return inputs, nil
}

func expand(p string) string {
	if rest, ok := strings.CutPrefix(p, "~/"); ok {
		if home, err := os.UserHomeDir(); err == nil {
			return filepath.Join(home, rest)
		}
	}
	return p
}

type machines struct {
	root     string
	mu       sync.Mutex
	extracts trialfacts.Extracts
	configs  map[string]sim.Config
}

func newMachines(root string) *machines {
	return &machines{root: root, extracts: trialfacts.Extracts{}, configs: make(map[string]sim.Config)}
}

func (m *machines) resolve(path string) (string, error) {
	if filepath.IsAbs(path) {
		return path, nil
	}
	for _, base := range []string{m.root, filepath.Join(m.root, "tools", "bench")} {
		p := filepath.Join(base, path)
		if _, err := os.Stat(p); err == nil {
			return p, nil
		}
	}
	return "", fmt.Errorf("machine file %q not found under %s or its tools/bench", path, m.root)
}

func (m *machines) config(path string) (sim.Config, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if cfg, ok := m.configs[path]; ok {
		return cfg, nil
	}
	var cfg sim.Config
	if path != "" {
		resolved, err := m.resolve(path)
		if err != nil {
			return cfg, err
		}
		if cfg, err = sim.LoadMachine(resolved); err != nil {
			return cfg, err
		}
		if cfg.Facts != "" {
			if cfg.Replay, err = m.extracts.Replay(resolved, cfg); err != nil {
				return cfg, err
			}
		}
	}
	m.configs[path] = cfg
	return cfg, nil
}

func readRecords(path string) ([]record, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	decoder := json.NewDecoder(f)
	var out []record
	for {
		var r record
		if err := decoder.Decode(&r); errors.Is(err, io.EOF) {
			return out, nil
		} else if err != nil {
			return nil, fmt.Errorf("decode %s: %w", path, err)
		}
		out = append(out, r)
	}
}

func runDir(keep string, r record) (string, bool) {
	name := fmt.Sprintf("%s-%d", r.Split, r.Seed)
	candidates, _ := filepath.Glob(filepath.Join(keep, "bench-*", r.Scenario, name))
	candidates = append(candidates, filepath.Join(keep, r.Scenario, name))
	slices.Sort(candidates)
	for _, c := range slices.Backward(candidates) {
		if _, err := os.Stat(filepath.Join(c, "events.jsonl")); err == nil {
			return c, true
		}
	}
	return "", false
}

func load(in input, machines *machines, jobs int) (*variant, validation, error) {
	records, err := readRecords(in.out)
	if err != nil {
		return nil, validation{}, err
	}
	v := &variant{name: in.name, runs: make(map[runKey]*run)}
	cfgs := make([]sim.Config, len(records))
	for i, r := range records {
		k := runKey{r.Scenario, r.Split, r.Seed}
		if v.runs[k] != nil {
			return nil, validation{}, fmt.Errorf("duplicate run %s/%s-%d", r.Scenario, r.Split, r.Seed)
		}
		if cfgs[i], err = machines.config(r.Machine); err != nil {
			return nil, validation{}, fmt.Errorf("run %s/%s-%d: %w", r.Scenario, r.Split, r.Seed, err)
		}
		v.runs[k] = &run{record: r}
		v.list = append(v.list, v.runs[k])
	}
	var val validation
	var mu sync.Mutex
	var firstErr error
	queue := make(chan int)
	var wg sync.WaitGroup
	for range max(1, jobs) {
		wg.Go(func() {
			for i := range queue {
				one, err := analyse(in.keep, v.list[i], cfgs[i])
				mu.Lock()
				val.add(one)
				if err != nil && firstErr == nil {
					firstErr = err
				}
				mu.Unlock()
			}
		})
	}
	for i := range v.list {
		queue <- i
	}
	close(queue)
	wg.Wait()
	return v, val, firstErr
}

func (v *validation) add(o validation) {
	v.runs += o.runs
	v.withJournal += o.withJournal
	v.profile += o.profile
	v.hazard += o.hazard
	v.simHours += o.simHours
	v.crashes += o.crashes
	v.hazardNone += o.hazardNone
}

func analyse(keep string, r *run, cfg sim.Config) (validation, error) {
	val := validation{runs: 1}
	dir, ok := runDir(keep, r.record)
	if !ok {
		return val, nil
	}
	events, _, err := journal.Read(dir)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return val, fmt.Errorf("read %s: %w", dir, err)
	}
	cfg.Seed = r.Seed
	m, err := sim.New(cfg)
	if err != nil {
		return val, fmt.Errorf("create machine for %s: %w", dir, err)
	}
	cores := cfg.Cores
	if cores == 0 {
		cores = 16
	}
	got := replay(events, cores)
	r.journal = true
	val.withJournal = 1
	if !slices.Equal(got.final, r.FinalProfile) {
		val.profile++
	}
	if got.simHours != r.SimHours {
		val.simHours++
	}
	if got.crashes != r.Crashes {
		val.crashes++
	}
	hazard := worstR7HazardPerH(m, got.final)
	switch {
	case hazard == nil && r.WorstR7HazardPerH == nil:
		val.hazardNone++
	case hazard == nil || r.WorstR7HazardPerH == nil || *hazard != *r.WorstR7HazardPerH:
		val.hazard++
	}
	for i, h := range checkpoints {
		profile, ok := lastConfirmedBy(got.confirmed, h)
		if !ok {
			continue
		}
		at := atCheckpoint{have: true, hazard: worstR7HazardPerH(m, profile)}
		for _, offset := range profile {
			at.depth += offset
		}
		r.confirm[i] = at
	}
	return val, nil
}

func percentile(sorted []float64, p float64) float64 {
	if len(sorted) == 0 {
		return 0
	}
	at := p * float64(len(sorted)-1)
	lo := int(at)
	hi := min(lo+1, len(sorted)-1)
	return sorted[lo] + (sorted[hi]-sorted[lo])*(at-float64(lo))
}

type dist []float64

func (d dist) quantile(p float64) (float64, bool) {
	if len(d) == 0 {
		return 0, false
	}
	s := slices.Sorted(slices.Values(d))
	return percentile(s, p), true
}

func (d dist) cell(p float64, format string) string {
	q, ok := d.quantile(p)
	if !ok {
		return "-"
	}
	return fmt.Sprintf(format, q)
}

type accuracy struct {
	pairs, cores              int
	abs                       float64
	within1, within2, shallow int
	deeper                    int
}

func compareToReference(v, ref *variant, scenario string) accuracy {
	var a accuracy
	for _, r := range v.list {
		if r.Scenario != scenario || r.Status != "concluded" {
			continue
		}
		base := ref.runs[runKey{r.Scenario, r.Split, r.Seed}]
		if base == nil || base.Status != "concluded" || len(base.FinalProfile) != len(r.FinalProfile) {
			continue
		}
		a.pairs++
		for i, offset := range r.FinalProfile {
			d := offset - base.FinalProfile[i]
			a.cores++
			a.abs += float64(abs(d))
			if abs(d) <= 1 {
				a.within1++
			}
			if abs(d) <= 2 {
				a.within2++
			}
			if d > 0 {
				a.shallow++
			}
			if d < 0 {
				a.deeper++
			}
		}
	}
	return a
}

func abs(x int) int {
	if x < 0 {
		return -x
	}
	return x
}

type summary struct {
	n                          int
	status                     map[string]int
	simHours, first            dist
	crashes, hunts, depth, haz dist
}

func summarise(v *variant, scenario string) summary {
	s := summary{status: make(map[string]int)}
	for _, r := range v.list {
		if r.Scenario != scenario {
			continue
		}
		s.n++
		s.status[r.Status]++
		if r.Status == "concluded" {
			s.simHours = append(s.simHours, r.SimHours)
		}
		if r.FirstPassedCycleH != nil {
			s.first = append(s.first, *r.FirstPassedCycleH)
		}
		s.crashes = append(s.crashes, float64(r.Crashes))
		s.hunts = append(s.hunts, float64(r.Hunts))
		s.depth = append(s.depth, float64(r.Depth))
		if r.WorstR7HazardPerH != nil {
			s.haz = append(s.haz, *r.WorstR7HazardPerH)
		}
	}
	return s
}

func (s summary) statusCell() string {
	var parts []string
	for _, name := range []string{"concluded", "deadend", "timeout", "censored", "error"} {
		parts = append(parts, fmt.Sprint(s.status[name]))
	}
	return strings.Join(parts, "/")
}

func scenarios(variants []*variant) []string {
	var names []string
	for _, v := range variants {
		for _, r := range v.list {
			if !slices.Contains(names, r.Scenario) {
				names = append(names, r.Scenario)
			}
		}
	}
	return names
}

func has(v *variant, scenario string) bool {
	return slices.ContainsFunc(v.list, func(r *run) bool { return r.Scenario == scenario })
}

func table(w io.Writer, header []string, rows [][]string) {
	fmt.Fprintf(w, "| %s |\n|%s\n", strings.Join(header, " | "), strings.Repeat(" --- |", len(header)))
	for _, row := range rows {
		fmt.Fprintf(w, "| %s |\n", strings.Join(row, " | "))
	}
	fmt.Fprintln(w)
}

func report(w io.Writer, variants []*variant, reference string, inputs []input, valid []validation) {
	ref := variants[0]
	fmt.Fprintln(w, "# #493 experiment comparison")
	fmt.Fprintf(w, "\nReference: `%s`. Runs pool the dev and holdout splits. Status counts are concluded/deadend/timeout/censored/error.\n", reference)
	fmt.Fprintln(w, "sim_h pools concluded runs; first_h, crashes, hunts, depth and R7 hazard pool all runs that have the value. Depth is the bench's sum of offsets (more negative = deeper). R7 hazard is the bench's worst_r7_hazard_per_h of the final profile, per hour.")
	fmt.Fprintln(w, "Accuracy pools every core of runs concluded in both variants at the same scenario, split and seed: diff = offset - reference offset, positive = shallower.")
	fmt.Fprintln(w, "Last confirmed profile: the profile of the latest passed checking cycle ending at or before the hour; `have` is the share of runs with one.")
	for _, scenario := range scenarios(variants) {
		fmt.Fprintf(w, "\n## %s\n\n", scenario)
		var runRows, accRows, confRows [][]string
		for _, v := range variants {
			if !has(v, scenario) {
				continue
			}
			s := summarise(v, scenario)
			runRows = append(runRows, []string{v.name, fmt.Sprint(s.n), s.statusCell(),
				s.simHours.cell(.5, "%.1f"), s.simHours.cell(.9, "%.1f"),
				s.first.cell(.5, "%.1f"), s.first.cell(.9, "%.1f"),
				s.crashes.cell(.5, "%.1f"), s.hunts.cell(.5, "%.1f"), s.depth.cell(.5, "%.0f"),
				s.haz.cell(.5, "%.2f"), s.haz.cell(.9, "%.2f")})
			if has(ref, scenario) {
				a := compareToReference(v, ref, scenario)
				if a.cores == 0 {
					accRows = append(accRows, []string{v.name, "0", "-", "-", "-", "-", "-"})
				} else {
					n := float64(a.cores)
					accRows = append(accRows, []string{v.name, fmt.Sprint(a.pairs), fmt.Sprintf("%.3f", a.abs/n),
						pct(a.within1, a.cores), pct(a.within2, a.cores), pct(a.shallow, a.cores), pct(a.deeper, a.cores)})
				}
			}
			row := []string{v.name}
			for i := range checkpoints {
				row = append(row, confirmedCells(v, scenario, i)...)
			}
			confRows = append(confRows, row)
		}
		table(w, []string{"variant", "runs", "status", "sim_h med", "sim_h p90", "first_h med", "first_h p90", "crashes med", "hunts med", "depth med", "R7 haz med", "R7 haz p90"}, runRows)
		fmt.Fprintln(w, "Accuracy vs reference:")
		fmt.Fprintln(w)
		table(w, []string{"variant", "pairs", "mean abs diff", "within ±1", "within ±2", "shallower", "deeper"}, accRows)
		fmt.Fprintln(w, "Last confirmed profile (depth med / R7 haz med / R7 haz p90):")
		fmt.Fprintln(w)
		header := []string{"variant"}
		for _, h := range checkpoints {
			header = append(header, fmt.Sprintf("%gh have", h), fmt.Sprintf("%gh depth", h), fmt.Sprintf("%gh haz med", h), fmt.Sprintf("%gh haz p90", h))
		}
		table(w, header, confRows)
	}
	gates(w, variants, ref)
	conclusions(w, variants, ref)
	fmt.Fprintln(w, "## Validation against the bench's recorded values")
	fmt.Fprintln(w)
	var rows [][]string
	for i, in := range inputs {
		v := valid[i]
		rows = append(rows, []string{in.name, fmt.Sprint(v.runs), fmt.Sprint(v.withJournal), fmt.Sprint(v.profile), fmt.Sprint(v.hazard), fmt.Sprint(v.hazardNone), fmt.Sprint(v.simHours), fmt.Sprint(v.crashes)})
	}
	table(w, []string{"variant", "runs", "journals read", "final_profile mismatches", "worst R7 hazard mismatches", "hazard unavailable in both", "sim_hours mismatches", "crashes mismatches"}, rows)
}

func pct(n, total int) string { return fmt.Sprintf("%.1f%%", 100*float64(n)/float64(total)) }

func confirmedCells(v *variant, scenario string, i int) []string {
	var total, have int
	var depth, haz dist
	for _, r := range v.list {
		if r.Scenario != scenario {
			continue
		}
		total++
		if c := r.confirm[i]; c.have {
			have++
			depth = append(depth, float64(c.depth))
			if c.hazard != nil {
				haz = append(haz, *c.hazard)
			}
		}
	}
	if total == 0 {
		return []string{"-", "-", "-", "-"}
	}
	return []string{pct(have, total), depth.cell(.5, "%.0f"), haz.cell(.5, "%.2f"), haz.cell(.9, "%.2f")}
}

func gates(w io.Writer, variants []*variant, ref *variant) {
	const scenario = "target-shared-voltage"
	fmt.Fprintf(w, "\n## Gate: %s (#493 Q24/Q28)\n\n", scenario)
	fmt.Fprintln(w, "Targets: median ~48 h and p90 ~72 h to conclusion, median ~24 h to first result (ballpark), median crashes < 426.5, worst R7 hazard median <= 4.91 and p90 <= 10.90.")
	fmt.Fprintln(w)
	var rows [][]string
	for _, v := range variants {
		if !has(v, scenario) {
			continue
		}
		s := summarise(v, scenario)
		rows = append(rows, []string{v.name, s.statusCell(),
			gate(s.simHours, .5, 48, "%.1f", true), gate(s.simHours, .9, 72, "%.1f", true), gate(s.first, .5, 24, "%.1f", true),
			gate(s.crashes, .5, 426.5, "%.1f", false), gate(s.haz, .5, 4.91, "%.2f", true), gate(s.haz, .9, 10.90, "%.2f", true)})
	}
	if len(rows) == 0 {
		fmt.Fprintf(w, "No %s runs in any variant.\n\n", scenario)
		return
	}
	table(w, []string{"variant", "status", "median sim_h (~48)", "p90 sim_h (~72)", "median first_h (~24)", "median crashes (<426.5)", "R7 haz median (<=4.91)", "R7 haz p90 (<=10.90)"}, rows)
	fmt.Fprintln(w, "`ok` meets the target, `miss` does not; sim_h and first_h targets are ballpark.")
}

func gate(d dist, p, target float64, format string, inclusive bool) string {
	q, ok := d.quantile(p)
	if !ok {
		return "n/a"
	}
	met := q < target
	if inclusive {
		met = q <= target
	}
	verdict := "miss"
	if met {
		verdict = "ok"
	}
	return fmt.Sprintf(format+" %s", q, verdict)
}

func conclusions(w io.Writer, variants []*variant, ref *variant) {
	fmt.Fprint(w, "\n## Conclusion on the scenarios that must not regress\n\n")
	fmt.Fprintln(w, "`regression` marks fewer concluded runs than the reference, any deadend/timeout/censored/error run the reference does not have, or a higher median R7 hazard than the reference.")
	fmt.Fprintln(w)
	for _, scenario := range []string{"idle-limit", "late-onset", "target-nonmember-mce"} {
		if !has(ref, scenario) {
			fmt.Fprintf(w, "### %s\n\nNo reference runs.\n\n", scenario)
			continue
		}
		fmt.Fprintf(w, "### %s\n\n", scenario)
		rs := summarise(ref, scenario)
		var rows [][]string
		for _, v := range variants {
			if !has(v, scenario) {
				continue
			}
			s := summarise(v, scenario)
			a := compareToReference(v, ref, scenario)
			acc := "-"
			if a.cores > 0 {
				acc = fmt.Sprintf("%.3f", a.abs/float64(a.cores))
			}
			verdict := "reference"
			if v != ref {
				verdict = "same as reference"
				var why []string
				if s.status["concluded"] < rs.status["concluded"] {
					why = append(why, "fewer concluded")
				}
				if s.n-s.status["concluded"] > rs.n-rs.status["concluded"] {
					why = append(why, "more unfinished runs")
				}
				vq, vok := s.haz.quantile(.5)
				rq, rok := rs.haz.quantile(.5)
				if vok && rok && vq > rq {
					why = append(why, "higher median R7 hazard")
				}
				if len(why) > 0 {
					verdict = "regression: " + strings.Join(why, ", ")
				} else if s.status["concluded"] > rs.status["concluded"] {
					verdict = "more concluded than reference"
				}
			}
			rows = append(rows, []string{v.name, fmt.Sprintf("%d/%d", s.status["concluded"], s.n), s.statusCell(), s.simHours.cell(.5, "%.1f"), s.haz.cell(.5, "%.2f"), acc, verdict})
		}
		table(w, []string{"variant", "concluded", "status", "sim_h med", "R7 haz med", "mean abs diff vs reference", "conclusion"}, rows)
	}
}
