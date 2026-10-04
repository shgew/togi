package journal

import (
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

func TestCheckingChainRoundTrip(t *testing.T) {
	p := &CheckingChain{Cycle: 2, Step: 3, CCD: 1, Workload: "avx2", Groups: [][]int{{8, 9}, {10}, {11, 12}}, Cores: []int{10, 11, 12}, SourceSeqs: []int{41, 43}, Profile: []int{-20, -30}, Part: "partial 1"}
	e := Event{Seq: 44, Time: time.Unix(100, 0), Boot: "boot", Kind: p.Kind(), Msg: p.Message(), Cause: p.SourceSeqs, Data: p}
	line, err := encode(e)
	if err != nil {
		t.Fatal(err)
	}
	got, err := decode(line)
	if err != nil {
		t.Fatal(err)
	}
	if got.Kind != KindCheckingChain || got.Msg != e.Msg {
		t.Fatalf("chain envelope lost: %+v", got)
	}
	if diff := cmp.Diff(p, got.Data); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(e.Cause, got.Cause); diff != "" {
		t.Fatal(diff)
	}
}

func TestConfigSnapshotPreservesHuntEvidence(t *testing.T) {
	p := &ConfigLoaded{Config: ConfigSnapshot{Evidence: ConfigEvidence{Miss: 0.01, Rate: 0.3}}}
	line, err := encode(Event{Seq: 1, Time: time.Unix(100, 0), Kind: p.Kind(), Msg: p.Message(), Data: p})
	if err != nil {
		t.Fatal(err)
	}
	got, err := decode(line)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(p, got.Data); diff != "" {
		t.Fatal(diff)
	}
}
