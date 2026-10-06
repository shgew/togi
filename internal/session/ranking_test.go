package session

import (
	"context"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type rankingHost struct {
	machine.Host
	values []int
}

func (h rankingHost) Ranking() ([]machine.CoreRank, error) {
	cores, err := h.Host.Topology()
	if err != nil {
		return nil, err
	}
	ranking := make([]machine.CoreRank, len(h.values))
	for i, value := range h.values {
		ranking[i] = machine.CoreRank{Core: cores[i].Core, Value: value}
	}
	return ranking, nil
}

func TestRankingFallbackAndTies(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name   string
		values []int
		want   []int
		detail string
	}{
		{"missing value", []int{4}, []int{0, 1, 2, 3}, "ranking has 1 values for 4 cores"},
		{"uniform", []int{7, 7, 7, 7}, []int{0, 1, 2, 3}, "every core ranks 7"},
		{"tie", []int{2, 5, 5, 1}, []int{1, 2, 0, 3}, ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			r, _, closeJournal := checkedRunner(t, []int{0, 0, 0, 0})
			defer closeJournal()
			for core := range 4 {
				if _, err := r.append(&journal.CorePhase{Core: core, To: journal.PhaseAtLimit, Offset: -50}); err != nil {
					t.Fatal(err)
				}
			}
			if _, err := r.append(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Together, Regime: machine.R6, Profile: []int{-50, -50, -50, -50}}); err != nil {
				t.Fatal(err)
			}
			r.in.Machine.Host = rankingHost{Host: r.in.Machine.Host, values: tc.values}
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			seq := 0
			r.in.Journal = &cancelAtDrainBoundary{Journal: r.in.Journal, cancel: cancel, seq: &seq, match: func(p journal.Payload) bool { return p.Kind() == journal.KindHostRanking }}
			stop, err := r.loop(ctx)
			if err != nil || stop.Reason != StopSignal || seq == 0 {
				t.Fatalf("ranking boundary: %+v, %v, seq %d", stop, err, seq)
			}
			p := r.in.Journal.Events()[seq-1].Data.(*journal.HostRanking)
			if diff := cmp.Diff(&journal.HostRanking{Ranking: tc.want, Values: tc.values, Detail: tc.detail}, p); diff != "" {
				t.Fatalf("preferred-core ranking (-want +got):\n%s", diff)
			}
		})
	}
}
