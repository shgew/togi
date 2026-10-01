package session

import (
	"encoding/json"
	"testing"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/machine"
)

func TestConfigSnapshotPreservesCompleteWireShape(t *testing.T) {
	for _, tc := range []struct {
		name   string
		config config.Config
		wire   string
	}{
		{"values", config.Config{
			StartOffsets: map[int]int{0: -17}, CandidateEdges: map[int]int{7: -32},
			Durations: config.Durations{SearchTrialS: 11, StartS: 12, GuardTrialS: 13, GuardIdleS: 14, GuardAllCoreS: 15},
			Evidence:  config.Evidence{Miss: 0.01, Rate: 0.2}, Guard: config.Guard{Rotation: []machine.Regime{machine.R7, machine.R1}},
			DeadEnds: config.DeadEnds{InconclusiveInARow: 4, StrayCrashesInARow: 5},
			Backends: config.Backends{Mprime: "/test/mprime", Ycruncher: "/test/ycruncher"}, BackendUser: "trial",
		}, `{"start_offsets":{"0":-17},"candidate_edges":{"7":-32},"durations":{"search_trial_s":11,"start_s":12,"guard_trial_s":13,"guard_idle_s":14,"guard_all_core_s":15},"evidence":{"miss":0.01,"rate":0.2},"guard":{"rotation":["R7","R1"]},"dead_ends":{"inconclusive_in_a_row":4,"stray_crashes_in_a_row":5},"backends":{"mprime":"/test/mprime","ycruncher":"/test/ycruncher"},"backend_user":"trial"}`},
		{"null and zero", config.Config{}, `{"start_offsets":null,"candidate_edges":null,"durations":{"search_trial_s":0,"start_s":0,"guard_trial_s":0,"guard_idle_s":0,"guard_all_core_s":0},"evidence":{"miss":0,"rate":0},"guard":{"rotation":null},"dead_ends":{"inconclusive_in_a_row":0,"stray_crashes_in_a_row":0},"backends":{"mprime":"","ycruncher":""},"backend_user":""}`},
		{"empty", config.Config{StartOffsets: map[int]int{}, CandidateEdges: map[int]int{}, Guard: config.Guard{Rotation: []machine.Regime{}}}, `{"start_offsets":{},"candidate_edges":{},"durations":{"search_trial_s":0,"start_s":0,"guard_trial_s":0,"guard_idle_s":0,"guard_all_core_s":0},"evidence":{"miss":0,"rate":0},"guard":{"rotation":[]},"dead_ends":{"inconclusive_in_a_row":0,"stray_crashes_in_a_row":0},"backends":{"mprime":"","ycruncher":""},"backend_user":""}`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			wire, err := json.Marshal(configSnapshot(tc.config))
			if err != nil {
				t.Fatal(err)
			}
			if string(wire) != tc.wire {
				t.Fatalf("config snapshot wire:\n%s\nwant:\n%s", wire, tc.wire)
			}
		})
	}
}
