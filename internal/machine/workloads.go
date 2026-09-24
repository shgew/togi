package machine

import "fmt"

type Backend string

const (
	Mprime    Backend = "mprime"
	Ycruncher Backend = "ycruncher"
)

type Workload struct {
	ID string
	// Base is the ID of the R1 or R2 workload this one derives from.
	Base    string
	Backend Backend
	Label   string
	Threads int
	DutyPct int
}

var baseR1 = []Workload{
	{ID: "mprime-sse-4k-21k", Backend: Mprime, Label: "mprime SSE 4K-21K", Threads: 1},
	{ID: "ycruncher-bkt-sftv4", Backend: Ycruncher, Label: "y-cruncher BKT + SFTv4", Threads: 1},
	{ID: "ycruncher-snt-svt", Backend: Ycruncher, Label: "y-cruncher SNT + SVT", Threads: 1},
}

var baseR2 = []Workload{
	{ID: "mprime-avx2-36k-248k", Backend: Mprime, Label: "mprime AVX2 36K-248K", Threads: 1},
	{ID: "mprime-avx512-36k-248k", Backend: Mprime, Label: "mprime AVX-512 36K-248K", Threads: 1},
	{ID: "ycruncher-fftv4-n63-vt3", Backend: Ycruncher, Label: "y-cruncher FFTv4 + N63 + VT3", Threads: 1},
}

var catalog = buildCatalog()

func buildCatalog() map[Regime][]Workload {
	for _, base := range [][]Workload{baseR1, baseR2} {
		for i := range base {
			base[i].Base = base[i].ID
		}
	}
	mixed := append(append([]Workload{}, baseR1...), baseR2...)
	derive := func(base []Workload, idSuffix, labelSuffix string, threads int) []Workload {
		out := make([]Workload, len(base))
		for i, w := range base {
			out[i] = Workload{ID: w.ID + idSuffix, Base: w.ID, Backend: w.Backend, Label: w.Label + labelSuffix, Threads: threads}
		}
		return out
	}
	duty := []struct{ w, pct int }{{0, 25}, {1, 50}, {2, 75}, {0, 50}, {1, 75}, {2, 25}, {0, 75}, {1, 25}, {2, 50}}
	r4 := make([]Workload, len(duty))
	for i, d := range duty {
		w := baseR1[d.w]
		r4[i] = Workload{
			ID:      fmt.Sprintf("%s-duty%d", w.ID, d.pct),
			Base:    w.ID,
			Backend: w.Backend,
			Label:   fmt.Sprintf("%s %d%% duty", w.Label, d.pct),
			Threads: 1,
			DutyPct: d.pct,
		}
	}
	return map[Regime][]Workload{
		R1: baseR1,
		R2: baseR2,
		R3: derive(mixed, "-steps", " load steps", 1),
		R4: r4,
		R5: derive(mixed, "-smt", " SMT pair", 2),
		R6: derive(baseR1, "-idle", " idle then bursts", 1),
		R7: derive(baseR2, "-allcore", " all-core", 1),
	}
}

func Workloads(r Regime) []Workload {
	return catalog[r]
}

func PickWorkload(r Regime, index int) Workload {
	ws := catalog[r]
	return ws[index%len(ws)]
}

func WorkloadByID(id string) (Workload, bool) {
	for _, ws := range catalog {
		for _, w := range ws {
			if w.ID == id {
				return w, true
			}
		}
	}
	return Workload{}, false
}
