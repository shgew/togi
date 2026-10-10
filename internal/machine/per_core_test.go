package machine

import (
	"encoding/json"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestPerCoreEncodesAsMapDoes(t *testing.T) {
	t.Parallel()
	for _, readings := range []map[int]int64{
		nil,
		{0: 0},
		{7: 5},
		{2: 2000, 7: 2000},
		{15: 1, 1: 2, 10: 3, 0: 4, 9: 5, 2: 6},
		{0: 1, 1: 1, 2: 1, 3: 1, 4: 1, 5: 1, 6: 1, 7: 1, 8: 1, 9: 1, 10: 1, 11: 1, 12: 1, 13: 1, 14: 1, 15: 1},
	} {
		type sample struct {
			ElapsedMS   int64          `json:"elapsed_ms"`
			WorkerCPUMS PerCore[int64] `json:"worker_cpu_ms,omitzero"`
		}
		type mapSample struct {
			ElapsedMS   int64         `json:"elapsed_ms"`
			WorkerCPUMS map[int]int64 `json:"worker_cpu_ms,omitempty"`
		}
		got, err := json.Marshal(sample{ElapsedMS: 1000, WorkerCPUMS: PerCoreFrom(readings)})
		if err != nil {
			t.Fatal(err)
		}
		want, err := json.Marshal(mapSample{ElapsedMS: 1000, WorkerCPUMS: readings})
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(string(want), string(got)); diff != "" {
			t.Errorf("%v: %s", readings, diff)
		}
		var decoded sample
		if err := json.Unmarshal(got, &decoded); err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(PerCoreFrom(readings), decoded.WorkerCPUMS); diff != "" {
			t.Errorf("%v round trip: %s", readings, diff)
		}
	}
}

func TestPerCoreRejectsUnsupportedCores(t *testing.T) {
	t.Parallel()
	var p PerCore[int]
	for _, core := range []int{-1, PerCoreMax} {
		if p.Set(core, 1) {
			t.Errorf("Set(%d) accepted", core)
		}
	}
	if !p.IsZero() {
		t.Errorf("rejected cores were recorded: %v", p)
	}
	for _, in := range []string{`{"16":1}`, `{"-1":1}`, `{"x":1}`} {
		if err := json.Unmarshal([]byte(in), &p); err == nil {
			t.Errorf("Unmarshal(%s) accepted", in)
		}
	}
}

func TestPerCoreCopiesAreIndependent(t *testing.T) {
	t.Parallel()
	var a PerCore[int]
	a.Set(3, 1)
	b := a
	b.Set(3, 2)
	b.Set(4, 5)
	if v, _ := a.Get(3); v != 1 || a.Len() != 1 {
		t.Errorf("copy changed the original: %v", a)
	}
	if _, ok := a.Get(4); ok {
		t.Error("original gained core 4")
	}
}
