//go:build integration

package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/BurntSushi/toml"
	"github.com/shgew/togi/internal/sim"
)

// hermeticGit points every Git command of the test, including the bench's own, at a fresh repository with one commit
// whose work tree is the current source root, so the run needs no checkout metadata: source archives have no .git.
// Host config, inherited repository overrides and the user's identity cannot reach it.
func hermeticGit(t *testing.T) {
	t.Helper()
	root, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR", "GIT_CONFIG", "GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT", "GIT_NAMESPACE", "GIT_CEILING_DIRECTORIES"} {
		t.Setenv(name, "")
		if err := os.Unsetenv(name); err != nil {
			t.Fatal(err)
		}
	}
	t.Setenv("GIT_CONFIG_NOSYSTEM", "1")
	t.Setenv("GIT_CONFIG_GLOBAL", os.DevNull)
	t.Setenv("GIT_AUTHOR_NAME", "Bench Test")
	t.Setenv("GIT_AUTHOR_EMAIL", "bench@example.com")
	t.Setenv("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
	t.Setenv("GIT_COMMITTER_NAME", "Bench Test")
	t.Setenv("GIT_COMMITTER_EMAIL", "bench@example.com")
	t.Setenv("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
	fixture := t.TempDir()
	t.Setenv("GIT_DIR", filepath.Join(fixture, ".git"))
	t.Setenv("GIT_WORK_TREE", fixture)
	for _, args := range [][]string{{"init", "--initial-branch=main", "--template="}, {"commit", "--allow-empty", "--no-verify", "-m", "Fixture"}} {
		if out, err := exec.Command("git", args...).CombinedOutput(); err != nil {
			t.Fatalf("git %s: %v: %s", strings.Join(args, " "), err, out)
		}
	}
	t.Setenv("GIT_WORK_TREE", root)
}

func TestGateCLIIntegration(t *testing.T) {
	// execute builds ./tools/sim from the repository root, as the bench command does.
	t.Chdir(filepath.Join("..", ".."))
	hermeticGit(t)
	dir := t.TempDir()
	writeTOML := func(t *testing.T, path string, value any) {
		t.Helper()
		var data bytes.Buffer
		if err := toml.NewEncoder(&data).Encode(value); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, data.Bytes(), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	invoke := func(suite, baseline, output string) (int, string, string) {
		args := []string{"--suite", suite, "--split", "dev", "--jobs", "1", "--timeout", "5m", "--cache", filepath.Join(dir, "cache"), "--out", output}
		if baseline != "" {
			args = append(args, "--baseline", baseline)
		}
		var stdout, stderr bytes.Buffer
		code := run(args, &stdout, &stderr)
		return code, stdout.String(), stderr.String()
	}

	cfg, _ := voltageMetricConfig()
	// Eliminate legacy failures and keep the rail safely above every threshold,
	// even at -50: the shared-voltage softplus underflows to zero, so no RNG draw
	// can cause a failure. Only the synthetic baseline hazard changes the score.
	for id, workload := range cfg.SharedVoltage.Workload {
		for core := range workload.Core {
			workload.Core[core].BaseV = 10
			workload.Core[core].ThresholdV = 1
		}
		cfg.SharedVoltage.Workload[id] = workload
	}
	writeTOML(t, filepath.Join(dir, "machine.toml"), struct {
		Cores         int                `toml:"cores"`
		Model         map[string]float64 `toml:"model"`
		SharedVoltage *sim.SharedVoltage `toml:"shared_voltage"`
	}{cfg.Cores, map[string]float64{"past_limit_rate": 0, "near_limit_rate": 0}, cfg.SharedVoltage})

	suite := suiteFile{Scenarios: []scenario{{Name: "tiny", Machine: "machine.toml", Dev: []uint64{1}}}}
	plainSuite := filepath.Join(dir, "plain.toml")
	writeTOML(t, plainSuite, suite)
	recordedPath := filepath.Join(dir, "recorded.jsonl")
	if code, stdout, stderr := invoke(plainSuite, "", recordedPath); code != 0 || stderr != "" {
		t.Fatalf("record fixture: exit %d\nstdout:\n%s\nstderr:\n%s", code, stdout, stderr)
	}
	recorded, err := readResults(recordedPath)
	if err != nil {
		t.Fatal(err)
	}
	if len(recorded) != 1 {
		t.Fatalf("recorded %d runs, want one", len(recorded))
	}
	original := recorded[0]
	if original.Status != "concluded" || original.ExitCode != 0 || original.SimHours <= 0 || original.WorstR7HazardPerH == nil || original.Commit == "" || original.Ruleset <= 0 {
		t.Fatalf("fixture must conclude with time, shared-voltage metrics and code identity: %+v", original)
	}

	suite.Gate = &gate{
		ID:            "cli-integration",
		Baseline:      gateBaseline{Ruleset: original.Ruleset, Commits: []string{original.Commit}},
		Split:         "dev",
		Gated:         []string{"tiny"},
		Pooled:        []string{"tiny"},
		Quantiles:     []float64{0.5, 0.9},
		Confidence:    0.95,
		Resamples:     10000,
		BootstrapSeed: [2]uint64{1, 1},
		MaxTimeRatio:  2,
	}
	gatedSuite := filepath.Join(dir, "gated.toml")
	writeTOML(t, gatedSuite, suite)

	passing := original
	passing.WorstR7HazardPerH = new(*original.WorstR7HazardPerH + 1)
	slow := passing
	slow.SimHours /= 3
	wrongRuleset := passing
	wrongRuleset.Ruleset++
	wrongCommit := passing
	wrongCommit.Commit += "-wrong"

	criteria := func(baseline result, timeLine, pooledVerdict string) []string {
		hazard, base := *original.WorstR7HazardPerH, *baseline.WorstR7HazardPerH
		change := hazard - base
		lines := []string{"gate conclusion tiny baseline_concluded=1 candidate_concluded=1 lost=0 lost_seeds=[] threshold=lost==0 PASS"}
		for _, label := range []string{"median", "p90"} {
			lines = append(lines, fmt.Sprintf("gate %s tiny baseline=%.6f candidate=%.6f change=%+.6f ci=[%+.6f,%+.6f] threshold=ci_low<=0 PASS", label, base, hazard, change, change, change))
		}
		lines = append(lines, timeLine, fmt.Sprintf("gate pooled_median tiny baseline=%.6f candidate=%.6f change=%+.6f threshold=change<0 %s", base, hazard, change, pooledVerdict))
		return lines
	}
	passCriteria := criteria(passing, "gate time tiny ratio=1.0000 timed=1 threshold=ratio<=2 PASS", "PASS")
	equalCriteria := criteria(original, "gate time tiny ratio=1.0000 timed=1 threshold=ratio<=2 PASS", "FAIL")
	slowCriteria := slices.Clone(passCriteria)
	slowCriteria[3] = "gate time tiny ratio=3.0000 timed=1 threshold=ratio<=2 FAIL"
	header := fmt.Sprintf("gate cli-integration: judged against ruleset %d at %s on every dev seed; quantiles of worst_r7_hazard_per_h fail only when the 95%% interval of candidate minus baseline (10000 paired resamples, PCG(1,1)) lies above 0; time is the geometric mean ratio over pairs both concluded.", original.Ruleset, original.Commit)

	for _, tc := range []struct {
		name       string
		baseline   []result
		noGate     bool
		exit       int
		generic    string
		criteria   []string
		gateResult string
	}{
		{"pass", []result{passing}, false, 0, "NEUTRAL", passCriteria, "gate cli-integration: PASS 5 criteria"},
		{"equality fails pooled improvement", []result{original}, false, 1, "NEUTRAL", equalCriteria, "gate cli-integration: FAIL 1 of 5 criteria"},
		{"time fails", []result{slow}, false, 1, "REJECT", slowCriteria, "gate cli-integration: FAIL 1 of 5 criteria"},
		{"missing required seed refused", nil, false, 1, "NEUTRAL", nil, "gate cli-integration: FAIL refused: incomplete pairing: tiny seed 1 has no baseline run; the gate needs every dev seed of its scenarios"},
		{"wrong ruleset refused", []result{wrongRuleset}, false, 1, "NEUTRAL", nil, fmt.Sprintf("gate cli-integration: FAIL refused: baseline run tiny/1 is ruleset %d at %s, not ruleset %d at %s", wrongRuleset.Ruleset, wrongRuleset.Commit, original.Ruleset, original.Commit)},
		{"wrong commit refused", []result{wrongCommit}, false, 1, "NEUTRAL", nil, fmt.Sprintf("gate cli-integration: FAIL refused: baseline run tiny/1 is ruleset %d at %s, not ruleset %d at %s", wrongCommit.Ruleset, wrongCommit.Commit, original.Ruleset, original.Commit)},
		{"no gate neutral", []result{original}, true, 0, "NEUTRAL", nil, ""},
		{"no gate reject remains diagnostic", []result{slow}, true, 0, "REJECT", nil, ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			caseDir := t.TempDir()
			baselinePath := filepath.Join(caseDir, "baseline.jsonl")
			var data bytes.Buffer
			for _, record := range tc.baseline {
				if err := json.NewEncoder(&data).Encode(record); err != nil {
					t.Fatal(err)
				}
			}
			if err := os.WriteFile(baselinePath, data.Bytes(), 0o600); err != nil {
				t.Fatal(err)
			}
			path := gatedSuite
			if tc.noGate {
				path = plainSuite
			}
			output := filepath.Join(caseDir, "candidate.jsonl")
			code, stdout, stderr := invoke(path, baselinePath, output)
			if code != tc.exit {
				t.Fatalf("exit %d, want %d\nstdout:\n%s\nstderr:\n%s", code, tc.exit, stdout, stderr)
			}
			wantErr := ""
			if tc.exit != 0 {
				wantErr = "bench: gate failed\n"
			}
			if stderr != wantErr {
				t.Fatalf("stderr = %q, want %q\nstdout:\n%s", stderr, wantErr, stdout)
			}
			candidate, err := readResults(output)
			if err != nil {
				t.Fatal(err)
			}
			if len(candidate) != 1 || candidate[0].Status != original.Status || candidate[0].SimHours != original.SimHours || candidate[0].WorstR7HazardPerH == nil || *candidate[0].WorstR7HazardPerH != *original.WorstR7HazardPerH {
				t.Fatalf("real simulator changed the fixture's status, time or hazard: got %+v, recorded %+v", candidate, original)
			}
			comparison := strings.Index(stdout, "\ncomparison:")
			generic := strings.Index(stdout, "\nverdict: "+tc.generic+" ")
			if comparison < 0 || generic < comparison {
				t.Fatalf("missing generic %s comparison:\n%s", tc.generic, stdout)
			}
			if tc.noGate {
				if strings.Contains(stdout, "\ngate ") {
					t.Fatalf("suite without a gate emitted a registered report:\n%s", stdout)
				}
				return
			}
			registered := strings.Index(stdout, "\n"+header+"\n")
			if registered < generic {
				t.Fatalf("registered gate must appear after the generic comparison:\n%s", stdout)
			}
			for _, line := range append(slices.Clone(tc.criteria), tc.gateResult) {
				if !strings.Contains(stdout, "\n"+line+"\n") {
					t.Errorf("missing report line %q:\n%s", line, stdout)
				}
			}
			if len(tc.criteria) == 0 && strings.Contains(stdout, "\ngate conclusion ") {
				t.Errorf("refused gate emitted scored criteria:\n%s", stdout)
			}
		})
	}
}
