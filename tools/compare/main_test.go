package main

import (
	"bytes"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
)

func TestSuiteListsEveryRunWithItsEnsembleMember(t *testing.T) {
	path := filepath.Join(t.TempDir(), "suite.json")
	const suite = `{"scenarios": [
		{"name": "plain", "machine": "", "dev": [2], "holdout": [101]},
		{"name": "ensemble", "machines": ["m/a.json", "m/b.json"], "dev": [1, 2, 3], "holdout": [101]}
	]}`
	if err := os.WriteFile(path, []byte(suite), 0o644); err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	if err := listSuite(path, &out); err != nil {
		t.Fatal(err)
	}
	dir := filepath.Dir(path)
	want := "ensemble\tdev\t1\t" + dir + "/m/a.json\n" +
		"ensemble\tdev\t2\t" + dir + "/m/b.json\n" +
		"ensemble\tdev\t3\t" + dir + "/m/a.json\n" +
		"ensemble\tholdout\t101\t" + dir + "/m/a.json\n" +
		"plain\tdev\t2\t-\n" +
		"plain\tholdout\t101\t-\n"
	if diff := cmp.Diff(want, out.String()); diff != "" {
		t.Fatalf("suite (-want +got):\n%s", diff)
	}
}

func TestStampedRestoresOnlyAZeroStampTheSourceLineHad(t *testing.T) {
	for _, tc := range []struct {
		name string
		raw  string
		mono int64
		want string
	}{
		{"zero stamp kept", `{"seq":4,"mono_ms":0,"kind":"x"}`, 0, `{"seq":4,"mono_ms":0,"time":"t"}`},
		{"no stamp stays absent", `{"seq":4,"kind":"x"}`, 0, `{"seq":4,"time":"t"}`},
		{"nonzero stamp already encoded", `{"seq":4,"mono_ms":9,"kind":"x"}`, 9, `{"seq":4,"time":"t"}`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := stamped(journal.Event{Seq: 4, Mono: tc.mono, Raw: []byte(tc.raw)}, []byte(`{"seq":4,"time":"t"}`))
			if diff := cmp.Diff(tc.want, string(got)); diff != "" {
				t.Fatalf("line (-want +got):\n%s", diff)
			}
		})
	}
}

func TestInputsListsTestdataNeitherFixturesNorGoldensCover(t *testing.T) {
	root := t.TempDir()
	for _, rel := range []string{
		"tools/bench/suite.json",
		"internal/carry/testdata/real-facts.json",
		"internal/journal/testdata/fuzz/FuzzX/seed",
		"internal/tuner/testdata/sub/deep/data.json",
		"internal/carry/testdata/journal.jsonl",
		"internal/carry/testdata/journal.jsonl.gz",
		"internal/carry/testdata/encoded.golden",
		"internal/watch/testdata/view.golden",
		"internal/watch/testdata/view.txt",
		"internal/other/testdata/data.json",
		"internal/carry/data.json",
		"tools/board/testdata/board.golden",
	} {
		path := filepath.Join(root, filepath.FromSlash(rel))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(rel), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	// Every root must exist, as in the checkout.
	for _, r := range append(slices.Clone(inputRoots[1:]), "internal/defect/testdata", "cmd/togi/testdata", "tools/replay/testdata", "tools/stats/testdata", "internal/watch/testdata") {
		if err := os.MkdirAll(filepath.Join(root, filepath.FromSlash(r)), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	var out bytes.Buffer
	if err := inputs(root, &out); err != nil {
		t.Fatal(err)
	}
	var got []string
	for line := range strings.SplitSeq(strings.TrimSuffix(out.String(), "\n"), "\n") {
		got = append(got, strings.Split(line, "\t")[0])
	}
	want := []string{
		// A golden outside goldenRoots has no other manifest, so it is an input.
		"internal/carry/testdata/encoded.golden",
		"internal/carry/testdata/real-facts.json",
		"internal/journal/testdata/fuzz/FuzzX/seed",
		"internal/tuner/testdata/sub/deep/data.json",
		"internal/watch/testdata/view.txt",
		"tools/bench/suite.json",
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("inputs (-want +got):\n%s", diff)
	}
}
