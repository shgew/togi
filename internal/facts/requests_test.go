package facts

import (
	"bytes"
	"compress/gzip"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestReadJournalSummarizesPersistedRequests(t *testing.T) {
	for _, layout := range []string{"live", "archive", "live compressed", "archive compressed"} {
		for _, count := range []int{0, 19, 20} {
			t.Run(fmt.Sprintf("%s/%d samples", layout, count), func(t *testing.T) {
				layout, compressed := strings.TrimSuffix(layout, " compressed"), strings.HasSuffix(layout, " compressed")
				dir := t.TempDir()
				path, trials := filepath.Join(dir, "events.jsonl"), filepath.Join(dir, "trials")
				if layout == "archive" {
					path, trials = filepath.Join(dir, "archive", "source.jsonl"), filepath.Join(dir, "archive", "source-trials")
					if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
						t.Fatal(err)
					}
				}
				writeJournal(t, path, []journal.Event{
					{Data: &journal.SessionStart{Schema: journal.Schema, Ruleset: 8, Session: "source", Cores: []machine.CoreInfo{{Core: 0, CCD: 9}, {Core: 1, CCD: 4}, {Core: 2, CCD: 9}}}},
					{Data: &journal.TrialIntent{Trial: "0001", Regime: machine.R7, Cores: []int{2, 0}, Profile: []int{-30, -40, -50}, Condition: machine.Together}},
					{Data: &journal.TrialEnd{Trial: "0001", Outcome: journal.OutcomePass}},
				}, "")
				if count > 0 {
					dir := filepath.Join(trials, "0001")
					if err := os.MkdirAll(dir, 0700); err != nil {
						t.Fatal(err)
					}
					warmup := "{\"elapsed_ms\":4999,\"pm_table\":{\"voltage_request_v\":[2,2,2]}}\n"
					sample := "{\"elapsed_ms\":5000,\"pm_table\":{\"voltage_request_v\":[1.125,2,1.25]},\"core_mhz\":{\"0\":4800,\"1\":5000,\"2\":4900}}\n"
					contents, name := []byte(strings.Repeat(warmup, 20)+strings.Repeat(sample, count)), "samples.jsonl"
					if compressed {
						var buf bytes.Buffer
						zw := gzip.NewWriter(&buf)
						if _, err := zw.Write(contents); err != nil {
							t.Fatal(err)
						}
						if err := zw.Close(); err != nil {
							t.Fatal(err)
						}
						contents, name = buf.Bytes(), name+".gz"
					}
					if err := os.WriteFile(filepath.Join(dir, name), contents, 0600); err != nil {
						t.Fatal(err)
					}
				}
				session, err := ReadJournal(path)
				if err != nil {
					t.Fatal(err)
				}
				f := session.Facts[0]
				var volts map[int]float64
				var top []int
				var mhz map[int]int
				if count == 20 {
					volts, top, mhz = map[int]float64{0: 1.125, 2: 1.25}, []int{2}, map[int]int{9: 4850}
				}
				if diff := cmp.Diff(volts, f.VoltageRequestsV); diff != "" {
					t.Fatal(diff)
				}
				if diff := cmp.Diff(top, f.TopRequesters); diff != "" {
					t.Fatal(diff)
				}
				if diff := cmp.Diff(mhz, f.CCDMHz); diff != "" {
					t.Fatal(diff)
				}
				if session.Trials[0].End.VoltageRequestsV != nil {
					t.Fatal("sample enrichment mutated the source trial.end")
				}
			})
		}
	}
}
