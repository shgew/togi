package session

import (
	"bytes"
	"compress/gzip"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/trial"
)

func requestSamples(count int) []machine.TrialConditions {
	warmup := &machine.PMTable{}
	warmup.VoltageRequestV[0], warmup.VoltageRequestV[1] = 2, 2
	samples := []machine.TrialConditions{{ElapsedMS: 4999, PMTable: warmup}, {ElapsedMS: 5000}}
	for i := range count {
		table := &machine.PMTable{}
		table.VoltageRequestV[0], table.VoltageRequestV[1], table.VoltageRequestV[15] = 1.125, 1.1255, 3
		samples = append(samples, machine.TrialConditions{ElapsedMS: int64(5000 + i*1000), PMTable: table, CoreMHz: machine.PerCoreFrom(map[int]int{0: 5000, 1: 5100, 15: 6000})})
	}
	return samples
}

func assertRequestTelemetry(t *testing.T, events []journal.Event, count int, recovered bool) {
	t.Helper()
	cores := map[int]machine.CoreInfo{}
	intents := map[string]*journal.TrialIntent{}
	seen := map[machine.Regime]bool{}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.SessionStart:
			for _, core := range p.Cores {
				cores[core.Core] = core
			}
		case *journal.TrialIntent:
			intents[p.Trial] = p
		case *journal.TrialEnd:
			if recovered && p.Trial != "0001" {
				continue
			}
			intent := intents[p.Trial]
			seen[intent.Regime] = true
			var wantRequests map[int]float64
			var wantTop []int
			var wantClocks map[int]int
			if count >= 20 {
				loaded := intent.Cores
				if len(loaded) == 0 {
					loaded = []int{*intent.Core}
				}
				wantRequests, wantClocks = map[int]float64{}, map[int]int{}
				perCCD := map[int][]int{}
				for _, core := range loaded {
					values := []float32{1.125, 1.1255}
					wantRequests[core] = float64(values[core])
					wantTop = append(wantTop, core)
					perCCD[cores[core].CCD] = append(perCCD[cores[core].CCD], 5000+100*core)
				}
				slices.Sort(wantTop)
				for ccd, clocks := range perCCD {
					if len(clocks) == 1 {
						wantClocks[ccd] = clocks[0]
					} else {
						wantClocks[ccd] = 5050
					}
				}
			}
			if diff := cmp.Diff(wantRequests, p.VoltageRequestsV); diff != "" {
				t.Fatalf("trial %s requests (-want +got):\n%s", p.Trial, diff)
			}
			if diff := cmp.Diff(wantTop, p.TopRequesters); diff != "" {
				t.Fatalf("trial %s top requesters (-want +got):\n%s", p.Trial, diff)
			}
			if diff := cmp.Diff(wantClocks, p.CCDMHz); diff != "" {
				t.Fatalf("trial %s clocks (-want +got):\n%s", p.Trial, diff)
			}
			var raw map[string]json.RawMessage
			if err := json.Unmarshal(e.Raw, &raw); err != nil {
				t.Fatal(err)
			}
			for _, field := range []string{"voltage_requests_v", "top_requesters", "ccd_mhz"} {
				if diff := cmp.Diff(count >= 20, raw[field] != nil); diff != "" {
					t.Fatalf("trial %s %s presence (-want +got):\n%s", p.Trial, field, diff)
				}
			}
			if p.Trial == "0001" {
				t.Log(string(e.Raw))
				if recovered && p.Signal != machine.Crash {
					t.Fatalf("recovered trial signal: %s", p.Signal)
				}
			}
		}
	}
	if recovered {
		if len(seen) != 1 {
			t.Fatal("missing recovered trial")
		}
	} else {
		want := map[machine.Regime]bool{machine.R1: true, machine.R2: true, machine.R3: true, machine.R4: true, machine.R5: true, machine.R6: true, machine.R7: true}
		if diff := cmp.Diff(want, seen); diff != "" {
			t.Fatal(diff)
		}
	}
}

func TestTrialRequestTelemetry(t *testing.T) {
	t.Parallel()
	for _, count := range []int{19, 20} {
		t.Run(fmt.Sprint(count), func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			seams := in.Machine.Seams()
			seams.Trials = voltageTrials{Trials: seams.Trials, samples: requestSamples(count)}
			if _, err := driveWithSeams(in, seams); err != nil {
				t.Fatal(err)
			}
			assertRequestTelemetry(t, readEvents(t, in.Dir), count, false)
		})
	}
}

// offlineCoreHost reports the topology without one core, as the kernel does
// when both of its logical CPUs are offline.
type offlineCoreHost struct {
	machine.Host
	core int
}

func (h offlineCoreHost) Topology() ([]machine.CoreInfo, error) {
	cores, err := h.Host.Topology()
	return slices.DeleteFunc(cores, func(c machine.CoreInfo) bool { return c.Core == h.core }), err
}

func TestCrashRecoveryRequestTelemetry(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		count   int
		offline bool
		gzip    bool
	}{
		{"19", 19, false, false},
		{"20", 20, false, false},
		{"20 in the compressed form", 20, false, true},
		// The recorded topology still maps a loaded core that is offline after the reset.
		{"20 with a loaded core offline", 20, true, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			in, _ := firstCrash(t, machine.ResetWatchdog, machine.Crash, false)
			dir := filepath.Join(in.Dir, "trials")
			trialDir := filepath.Join(dir, "0001")
			if err := os.MkdirAll(trialDir, 0755); err != nil {
				t.Fatal(err)
			}
			var contents []byte
			for _, sample := range requestSamples(tc.count) {
				line, err := json.Marshal(sample)
				if err != nil {
					t.Fatal(err)
				}
				contents = append(contents, append(line, '\n')...)
			}
			contents = append(contents, []byte(`{"elapsed_ms":100000,"pm_table":`)...)
			name := "samples.jsonl"
			if tc.gzip {
				contents, name = gzipBytes(t, contents), name+".gz"
			}
			if err := os.WriteFile(filepath.Join(trialDir, name), contents, 0644); err != nil {
				t.Fatal(err)
			}
			logDir := filepath.Join(trialDir, "work")
			if err := os.Mkdir(logDir, 0755); err != nil {
				t.Fatal(err)
			}
			for _, name := range []string{"stdout.log", "stderr.log"} {
				if err := os.WriteFile(filepath.Join(logDir, name), []byte("interrupted output\n"), 0644); err != nil {
					t.Fatal(err)
				}
			}
			seams := in.Machine.Seams()
			seams.Trials = sampledTrials{Trials: seams.Trials, reader: trial.New(trial.Options{Dir: dir})}
			if tc.offline {
				seams.Host = offlineCoreHost{Host: seams.Host, core: 0}
			}
			if _, err := driveWithSeams(in, seams); err != nil && !tc.offline {
				t.Fatal(err)
			}
			assertRequestTelemetry(t, readEvents(t, in.Dir), tc.count, true)
			if !tc.offline {
				for _, plain := range []string{filepath.Join(trialDir, "samples.jsonl"), filepath.Join(logDir, "stdout.log"), filepath.Join(logDir, "stderr.log")} {
					if _, err := os.Stat(plain); !os.IsNotExist(err) {
						t.Errorf("recovery left plain file %s: %v", filepath.Base(plain), err)
					}
					if _, err := os.Stat(plain + ".gz"); err != nil {
						t.Errorf("recovery compressed file %s: %v", filepath.Base(plain), err)
					}
				}
			}
		})
	}
}

func gzipBytes(t *testing.T, data []byte) []byte {
	t.Helper()
	var buf bytes.Buffer
	zw := gzip.NewWriter(&buf)
	if _, err := zw.Write(data); err != nil {
		t.Fatal(err)
	}
	if err := zw.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}
