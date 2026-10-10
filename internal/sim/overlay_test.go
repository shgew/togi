package sim

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

const (
	overlayAVX2   = "mprime-avx2-36k-248k-allcore"
	overlayAVX512 = "mprime-avx512-36k-248k-allcore"
)

func writeMachineFile(t *testing.T, path, content string) string {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

// writeParent writes the shared-voltage example machine to path, with extra members spliced in after its opening brace.
func writeParent(t *testing.T, path, extra string) string {
	t.Helper()
	source, err := os.ReadFile("../../tools/bench/machines/shared-voltage.json")
	if err != nil {
		t.Fatal(err)
	}
	content := string(source)
	if extra != "" {
		content = "{" + extra + strings.TrimPrefix(content, "{")
	}
	return writeMachineFile(t, path, content)
}

func overlayJSON(extends, entries string) string {
	return fmt.Sprintf(`{"description": "fixture overlay", "extends": %q, "shared_voltage_override": [%s]}`, extends, entries)
}

func loadMachineFor(t *testing.T, path string) Config {
	t.Helper()
	cfg, err := LoadMachine(path)
	if err != nil {
		t.Fatal(err)
	}
	return cfg
}

func TestOverlayDecodesToParentPlusNamedFields(t *testing.T) {
	dir := t.TempDir()
	parent := writeParent(t, filepath.Join(dir, "parent.json"), `"facts": "facts.gz",`)
	leaf := writeMachineFile(t, filepath.Join(dir, "leaf.json"), overlayJSON("parent.json", fmt.Sprintf(`
		{"workload": %q, "core": 5, "threshold_clock_v_per_100mhz": 0.06},
		{"workload": %q, "core": 9, "threshold_v": 1.18},
		{"workload": %q, "core": 10, "threshold_v": 1.22, "threshold_clock_v_per_100mhz": 0.05}`, overlayAVX2, overlayAVX512, overlayAVX512)))

	want := loadMachineFor(t, parent).Clone()
	w := want.SharedVoltage.Workload
	w[overlayAVX2].Core[5].ThresholdClockVPer100MHz = 0.06
	w[overlayAVX512].Core[9].ThresholdV = 1.18
	w[overlayAVX512].Core[10].ThresholdV = 1.22
	w[overlayAVX512].Core[10].ThresholdClockVPer100MHz = 0.05

	got, files, err := LoadMachineWithFiles(leaf)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(want, got, cmp.AllowUnexported(Replay{})); diff != "" {
		t.Fatalf("overlay is not its parent plus the named fields (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]string{leaf, parent}, files); diff != "" {
		t.Fatalf("machine files, leaf first (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(got, loadMachineFor(t, leaf), cmp.AllowUnexported(Replay{})); diff != "" {
		t.Fatalf("LoadMachine and LoadMachineWithFiles differ (-LoadMachine +WithFiles):\n%s", diff)
	}
	if _, err := New(got); err != nil {
		t.Fatalf("overlay machine is not a valid simulator: %v", err)
	}
	// The overlay's edits are its own: loading the parent again still gives the parent's values.
	untouched := loadMachineFor(t, parent)
	if untouched.SharedVoltage.Workload[overlayAVX2].Core[5].ThresholdClockVPer100MHz == 0.06 {
		t.Fatal("overlay changed its parent's decoded machine")
	}
}

func TestOverlayFollowsParentChanges(t *testing.T) {
	dir := t.TempDir()
	parent := writeParent(t, filepath.Join(dir, "parent.json"), "")
	leaf := writeMachineFile(t, filepath.Join(dir, "leaf.json"), overlayJSON("parent.json", fmt.Sprintf(`{"workload": %q, "core": 5, "threshold_v": 1.1}`, overlayAVX2)))
	if loadMachineFor(t, leaf).OldKernel {
		t.Fatal("fixture parent already has old_kernel")
	}
	writeParent(t, parent, `"old_kernel": true,`)
	got := loadMachineFor(t, leaf)
	if !got.OldKernel {
		t.Fatal("overlay did not inherit its parent's later change")
	}
	if got.SharedVoltage.Workload[overlayAVX2].Core[5].ThresholdV != 1.1 {
		t.Fatal("overlay lost its own override")
	}
}

func TestOverlayChainLoadsLeafFirstAndLastOverrideWins(t *testing.T) {
	dir := t.TempDir()
	root := writeParent(t, filepath.Join(dir, "root.json"), "")
	mid := writeMachineFile(t, filepath.Join(dir, "mid.json"), overlayJSON("root.json", fmt.Sprintf(`
		{"workload": %q, "core": 5, "threshold_v": 1.05, "threshold_clock_v_per_100mhz": 0.03},
		{"workload": %q, "core": 6, "threshold_v": 1.06}`, overlayAVX2, overlayAVX2)))
	leaf := writeMachineFile(t, filepath.Join(dir, "leaf.json"), overlayJSON("mid.json", fmt.Sprintf(`{"workload": %q, "core": 5, "threshold_clock_v_per_100mhz": 0.06}`, overlayAVX2)))

	got, files, err := LoadMachineWithFiles(leaf)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{leaf, mid, root}, files); diff != "" {
		t.Fatalf("machine files, leaf first (-want +got):\n%s", diff)
	}
	core5 := got.SharedVoltage.Workload[overlayAVX2].Core[5]
	if core5.ThresholdV != 1.05 || core5.ThresholdClockVPer100MHz != 0.06 {
		t.Fatalf("core 5 = %+v, want the middle overlay's threshold and the leaf's clock coefficient", core5)
	}
	if got.SharedVoltage.Workload[overlayAVX2].Core[6].ThresholdV != 1.06 {
		t.Fatal("leaf lost the middle overlay's override")
	}
}

func TestOverlayParentLookupIsRelativeToTheOverlay(t *testing.T) {
	dir := t.TempDir()
	parent := writeParent(t, filepath.Join(dir, "shared", "parent.json"), "")
	entry := fmt.Sprintf(`{"workload": %q, "core": 5, "threshold_v": 1.1}`, overlayAVX2)
	for _, tc := range []struct{ name, leaf, extends string }{
		{"child directory", filepath.Join(dir, "leaf.json"), "shared/parent.json"},
		{"sibling directory", filepath.Join(dir, "other", "leaf.json"), "../shared/parent.json"},
		{"dot segments", filepath.Join(dir, "a", "b", "leaf.json"), "./../../shared/parent.json"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			leaf := writeMachineFile(t, tc.leaf, overlayJSON(tc.extends, entry))
			_, files, err := LoadMachineWithFiles(leaf)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff([]string{leaf, parent}, files); diff != "" {
				t.Fatalf("machine files (-want +got):\n%s", diff)
			}
		})
	}
}

func TestOverlayRebasesInheritedFacts(t *testing.T) {
	dir := t.TempDir()
	writeParent(t, filepath.Join(dir, "machines", "parent.json"), `"facts": "../facts/extract.jsonl.gz",`)
	entry := fmt.Sprintf(`{"workload": %q, "core": 5, "threshold_v": 1.1}`, overlayAVX2)
	for _, tc := range []struct{ name, leaf, extends, want string }{
		{"same directory keeps the value", filepath.Join(dir, "machines", "leaf.json"), "parent.json", "../facts/extract.jsonl.gz"},
		{"parent directory", filepath.Join(dir, "leaf.json"), "machines/parent.json", "facts/extract.jsonl.gz"},
		{"sibling directory", filepath.Join(dir, "overlays", "leaf.json"), "../machines/parent.json", "../facts/extract.jsonl.gz"},
		{"deeper directory", filepath.Join(dir, "x", "y", "leaf.json"), "../../machines/parent.json", "../../facts/extract.jsonl.gz"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			leaf := writeMachineFile(t, tc.leaf, overlayJSON(tc.extends, entry))
			cfg := loadMachineFor(t, leaf)
			if cfg.Facts != tc.want {
				t.Fatalf("facts = %q, want %q", cfg.Facts, tc.want)
			}
			resolved := filepath.Join(filepath.Dir(leaf), cfg.Facts)
			if want := filepath.Join(dir, "facts", "extract.jsonl.gz"); resolved != want {
				t.Fatalf("facts resolve to %s from the overlay, want %s", resolved, want)
			}
		})
	}

	t.Run("chain across directories", func(t *testing.T) {
		mid := writeMachineFile(t, filepath.Join(dir, "mid", "mid.json"), overlayJSON("../machines/parent.json", entry))
		leaf := writeMachineFile(t, filepath.Join(dir, "a", "b", "leaf.json"), overlayJSON("../../mid/mid.json", entry[:len(entry)-1]+`, "threshold_clock_v_per_100mhz": 0.02}`))
		if got := loadMachineFor(t, mid).Facts; got != "../facts/extract.jsonl.gz" {
			t.Fatalf("middle facts = %q", got)
		}
		if got := loadMachineFor(t, leaf).Facts; got != "../../facts/extract.jsonl.gz" {
			t.Fatalf("leaf facts = %q", got)
		}
	})
}

func TestOverlayKeepsFactsAbsent(t *testing.T) {
	dir := t.TempDir()
	writeParent(t, filepath.Join(dir, "parent.json"), "")
	leaf := writeMachineFile(t, filepath.Join(dir, "x", "leaf.json"), overlayJSON("../parent.json", fmt.Sprintf(`{"workload": %q, "core": 5, "threshold_v": 1.1}`, overlayAVX2)))
	if got := loadMachineFor(t, leaf).Facts; got != "" {
		t.Fatalf("facts = %q for a parent without facts", got)
	}
}

func TestLoadMachineWithFilesFullMachine(t *testing.T) {
	dir := t.TempDir()
	path := writeParent(t, filepath.Join(dir, "full.json"), "")
	cfg, files, err := LoadMachineWithFiles(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{path}, files); diff != "" {
		t.Fatalf("machine files (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(loadMachineFor(t, path), cfg, cmp.AllowUnexported(Replay{})); diff != "" {
		t.Fatalf("LoadMachine and LoadMachineWithFiles differ (-LoadMachine +WithFiles):\n%s", diff)
	}
	if _, files, err := LoadMachineWithFiles(filepath.Join(dir, "missing.json")); err == nil || files != nil {
		t.Fatalf("missing machine: files %v, error %v", files, err)
	}
}

func TestOverlayRejectsCycles(t *testing.T) {
	entry := fmt.Sprintf(`{"workload": %q, "core": 5, "threshold_v": 1.1}`, overlayAVX2)
	dir := t.TempDir()
	self := writeMachineFile(t, filepath.Join(dir, "self.json"), overlayJSON("self.json", entry))
	a := writeMachineFile(t, filepath.Join(dir, "a.json"), overlayJSON("b.json", entry))
	writeMachineFile(t, filepath.Join(dir, "b.json"), overlayJSON("sub/../a.json", entry))
	c := writeMachineFile(t, filepath.Join(dir, "c.json"), overlayJSON("a.json", entry))
	// A directory symlink to its own directory gives the same file endless distinct lexical paths.
	loopDir := filepath.Join(dir, "loop")
	if err := os.MkdirAll(loopDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(".", filepath.Join(loopDir, "alias")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	alias := writeMachineFile(t, filepath.Join(loopDir, "leaf.json"), overlayJSON("alias/leaf.json", entry))
	deep := writeMachineFile(t, filepath.Join(loopDir, "deep.json"), overlayJSON("alias/alias/leaf.json", entry))
	for name, path := range map[string]string{"self": self, "two files": a, "overlay of a cycle": c, "symlinked directory alias": alias, "overlay of a symlink alias cycle": deep} {
		t.Run(name, func(t *testing.T) {
			cfg, files, err := LoadMachineWithFiles(path)
			if err == nil || !strings.Contains(err.Error(), "cycle") || files != nil || cfg.SharedVoltage != nil {
				t.Fatalf("cycle load: cfg %+v files %v error %v", cfg, files, err)
			}
		})
	}
}

func TestOverlayRejectsMalformedDocuments(t *testing.T) {
	dir := t.TempDir()
	writeParent(t, filepath.Join(dir, "parent.json"), "")
	writeMachineFile(t, filepath.Join(dir, "plain.json"), `{"cores": 16}`)
	writeMachineFile(t, filepath.Join(dir, "broken.json"), `{"cores": [`)
	entry := func(workload string, core any, rest string) string {
		return fmt.Sprintf(`{"workload": %q, "core": %v%s}`, workload, core, rest)
	}
	valid := entry(overlayAVX2, 5, `, "threshold_v": 1.1`)
	for _, tc := range []struct{ name, content, want string }{
		{"mixed with full machine fields", `{"cores": 16, "extends": "parent.json", "shared_voltage_override": [` + valid + `]}`, "unknown"},
		{"mixed with explicit zero cores", `{"cores": 0, "extends": "parent.json", "shared_voltage_override": [` + valid + `]}`, "unknown"},
		{"mixed with explicit false old_kernel", `{"old_kernel": false, "extends": "parent.json", "shared_voltage_override": [` + valid + `]}`, "unknown"},
		{"mixed with empty model", `{"model": {}, "extends": "parent.json", "shared_voltage_override": [` + valid + `]}`, "unknown"},
		{"mixed with null member", `{"facts": null, "extends": "parent.json", "shared_voltage_override": [` + valid + `]}`, "unknown"},
		{"mixed with shared_voltage", `{"extends": "parent.json", "shared_voltage": {}, "shared_voltage_override": [` + valid + `]}`, "unknown"},
		{"mixed with model", `{"extends": "parent.json", "model": {"growth": 2}, "shared_voltage_override": [` + valid + `]}`, "unknown"},
		{"override without extends", `{"shared_voltage_override": [` + valid + `]}`, "non-empty extends"},
		{"empty extends", `{"extends": "", "shared_voltage_override": [` + valid + `]}`, "non-empty extends"},
		{"null extends", `{"extends": null, "shared_voltage_override": [` + valid + `]}`, "non-empty extends"},
		{"empty extends without override", `{"extends": ""}`, "non-empty extends"},
		{"null extends without override", `{"extends": null}`, "non-empty extends"},
		{"extends without override", `{"extends": "parent.json"}`, "changes nothing"},
		{"null override", `{"extends": "parent.json", "shared_voltage_override": null}`, "changes nothing"},
		{"null override without extends", `{"shared_voltage_override": null}`, "non-empty extends"},
		{"empty override", `{"extends": "parent.json", "shared_voltage_override": []}`, "changes nothing"},
		{"absolute extends", fmt.Sprintf(`{"extends": %q, "shared_voltage_override": [%s]}`, filepath.Join(dir, "parent.json"), valid), "must be relative"},
		{"missing parent", overlayJSON("absent.json", valid), "absent.json"},
		{"broken parent", overlayJSON("broken.json", valid), "broken.json"},
		{"parent without shared_voltage", overlayJSON("plain.json", valid), "no shared_voltage"},
		{"unknown workload", overlayJSON("parent.json", entry("nope", 5, `, "threshold_v": 1.1`)), `unknown workload "nope"`},
		{"missing workload", overlayJSON("parent.json", `{"core": 5, "threshold_v": 1.1}`), `unknown workload ""`},
		{"core past the end", overlayJSON("parent.json", entry(overlayAVX2, 16, `, "threshold_v": 1.1`)), "unknown core 16"},
		{"negative core", overlayJSON("parent.json", entry(overlayAVX2, -1, `, "threshold_v": 1.1`)), "unknown core -1"},
		{"missing core", overlayJSON("parent.json", fmt.Sprintf(`{"workload": %q, "threshold_v": 1.1}`, overlayAVX2)), "needs core"},
		{"fractional core", overlayJSON("parent.json", entry(overlayAVX2, 5.5, `, "threshold_v": 1.1`)), "load simulator machine"},
		{"no effective field", overlayJSON("parent.json", entry(overlayAVX2, 5, "")), "sets neither"},
		{"duplicate target", overlayJSON("parent.json", valid+","+entry(overlayAVX2, 5, `, "threshold_clock_v_per_100mhz": 0.06`)), "repeats workload " + overlayAVX2 + " core 5"},
		{"zero threshold", overlayJSON("parent.json", entry(overlayAVX2, 5, `, "threshold_v": 0`)), "invalid voltage parameters"},
		{"negative threshold", overlayJSON("parent.json", entry(overlayAVX2, 5, `, "threshold_v": -1`)), "invalid voltage parameters"},
		{"negative clock coefficient", overlayJSON("parent.json", entry(overlayAVX2, 5, `, "threshold_clock_v_per_100mhz": -0.01`)), "invalid voltage parameters"},
		{"unknown entry member", overlayJSON("parent.json", entry(overlayAVX2, 5, `, "base_v": 1.1`)), "unknown"},
		{"unknown overlay member", `{"extends": "parent.json", "patch": [], "shared_voltage_override": [` + valid + `]}`, "unknown"},
		{"null threshold only", overlayJSON("parent.json", entry(overlayAVX2, 5, `, "threshold_v": null`)), "sets neither"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := writeMachineFile(t, filepath.Join(dir, "leaf.json"), tc.content)
			_, files, err := LoadMachineWithFiles(path)
			if err == nil || !strings.Contains(err.Error(), tc.want) || !strings.Contains(err.Error(), path) || files != nil {
				t.Fatalf("load error = %v (files %v), want path and %q", err, files, tc.want)
			}
		})
	}
}
