package main

import (
	"bytes"
	"compress/gzip"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestStatsArgumentErrors(t *testing.T) {
	for _, tc := range []struct {
		args []string
		want string
	}{
		{[]string{"unexpected"}, "stats: unexpected arguments: [unexpected]"},
		{[]string{"--since", "not-a-time"}, "stats: --since:"},
		{[]string{"--unknown"}, "flag provided but not defined: -unknown"},
	} {
		t.Run(fmt.Sprint(tc.args), func(t *testing.T) {
			var out, errOut bytes.Buffer
			err := run(tc.args, &out, &errOut)
			if err == nil || len(err.Error()) < len(tc.want) || err.Error()[:len(tc.want)] != tc.want {
				t.Fatalf("error = %v, want %q", err, tc.want)
			}
			if out.Len() != 0 {
				t.Fatalf("rendered report for rejected arguments: %s", &out)
			}
		})
	}
}

func TestStatsReadsOnlyJournal(t *testing.T) {
	f, err := os.Open(filepath.Join("testdata", "combination.jsonl.gz"))
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	compressed, err := gzip.NewReader(f)
	if err != nil {
		t.Fatal(err)
	}
	defer compressed.Close()
	data, err := io.ReadAll(compressed)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")
	if err := os.WriteFile(path, append(data, []byte("{\"unfinished\":")...), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "state.json"), []byte("invalid state"), 0o600); err != nil {
		t.Fatal(err)
	}
	want, err := os.ReadFile(filepath.Join("testdata", "combination.golden"))
	if err != nil {
		t.Fatal(err)
	}
	for _, args := range [][]string{{"--state-dir", dir}, {"--journal", path, "--state-dir", filepath.Join(dir, "absent")}} {
		var out, errOut bytes.Buffer
		if err := run(args, &out, &errOut); err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(string(want), out.String()); diff != "" {
			t.Fatal(diff)
		}
		if errOut.Len() != 0 {
			t.Fatalf("diagnostics = %s", &errOut)
		}
	}
	after, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(append(data, []byte("{\"unfinished\":")...)), string(after)); diff != "" {
		t.Fatalf("read modified journal: %s", diff)
	}
}
