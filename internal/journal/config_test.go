package journal

import (
	"bufio"
	"compress/gzip"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestConfigLoadedDecodesEveryShippedJournalBody(t *testing.T) {
	for _, tc := range []struct {
		name       string
		schema     int
		candidates map[int]int
		lap        []machine.Regime
	}{
		{"20260924T204352Z", 1, nil, []machine.Regime{machine.R1, machine.R2, machine.R6, machine.R3, machine.R4, machine.R7, machine.R5, machine.R6}},
		{"20260926T151414Z", 2, map[int]int{0: -36, 1: -38, 2: -38, 3: -36, 4: -36, 5: -39, 6: -46, 7: -46, 8: -50, 9: -50, 10: -50, 11: -47, 12: -50, 13: -49, 14: -50, 15: -50}, []machine.Regime{machine.R1, machine.R2, machine.R6, machine.R3, machine.R4, machine.R7, machine.R5, machine.R6}},
		{"20260927T221954Z", 2, map[int]int{0: -31, 1: -38, 2: -37, 3: -34, 4: -36, 5: -36, 6: -45, 7: -43, 8: -48, 9: -48, 10: -49, 11: -46, 12: -50, 13: -48, 14: -50, 15: -50}, []machine.Regime{machine.R2, machine.R7, machine.R6, machine.R5, machine.R1, machine.R3, machine.R4, machine.R6}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f, err := os.Open(filepath.Join("..", "carry", "testdata", tc.name+".jsonl.gz"))
			if err != nil {
				t.Fatal(err)
			}
			defer f.Close()
			gz, err := gzip.NewReader(f)
			if err != nil {
				t.Fatal(err)
			}
			defer gz.Close()
			want := ConfigSnapshot{
				StartOffsets: map[int]int{}, CandidateSoloLimits: tc.candidates,
				Durations: ConfigDurations{SearchTrialS: 90, CheckingTrialS: 120, CheckingIdleS: 900, CheckingAllCoreS: 1200},
				Checking:  ConfigChecking{Lap: tc.lap},
				DeadEnds:  ConfigDeadEnds{InconclusiveInARow: 3, StrayCrashesInARow: 3},
				Backends:  ConfigBackends{Mprime: "/nix/store/64hjzgj1msiyndpdxrk9l3gkjf3sczgj-mprime-31.04b02", Ycruncher: "/nix/store/n5g91xa9pzcfqyyh62xpwz3v53f87y0a-y-cruncher-0.8.7.9547"},
			}
			scan := bufio.NewScanner(gz)
			found := false
			for scan.Scan() {
				var kind struct {
					Kind   Kind `json:"kind"`
					Schema int  `json:"schema"`
				}
				if err := json.Unmarshal(scan.Bytes(), &kind); err != nil {
					t.Fatal(err)
				}
				if kind.Kind == KindSessionStart && kind.Schema != tc.schema {
					t.Fatalf("schema %d, want %d", kind.Schema, tc.schema)
				}
				if kind.Kind != KindConfigLoaded {
					continue
				}
				found = true
				line, err := translateLegacy(scan.Bytes())
				if err != nil {
					t.Fatal(err)
				}
				e, err := decode(line)
				if err != nil {
					t.Fatal(err)
				}
				p := e.Data.(*ConfigLoaded)
				if diff := cmp.Diff(want, p.Config); diff != "" {
					t.Fatalf("config.loaded #%d (-want +got):\n%s", e.Seq, diff)
				}
				if p.Path != "/etc/togi/config.toml" || !p.File {
					t.Fatalf("config source #%d: %+v", e.Seq, p)
				}
			}
			if err := scan.Err(); err != nil {
				t.Fatal(err)
			}
			if !found {
				t.Fatal("fixture has no config.loaded body")
			}
		})
	}
}
