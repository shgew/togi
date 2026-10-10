package machine

import (
	"compress/gzip"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func writeSamplesGzip(t *testing.T, path, content string) {
	t.Helper()
	f, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	zw := gzip.NewWriter(f)
	if _, err := zw.Write([]byte(content)); err != nil {
		t.Fatal(err)
	}
	if err := zw.Close(); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestReadSamplesForms(t *testing.T) {
	t.Parallel()
	const two = "{\"elapsed_ms\":1000}\n{\"elapsed_ms\":2000}\n"
	both := []TrialConditions{{ElapsedMS: 1000}, {ElapsedMS: 2000}}
	for _, tt := range []struct {
		name  string
		plain *string
		gz    *string
		rawGz bool // write gz as is instead of compressing it
		want  []TrialConditions
	}{
		{"plain", new(two), nil, false, both},
		{"compressed", nil, new(two), false, both},
		{"plain wins over a torn compressed file from a crash while compressing", new(two), new("torn"), true, both},
		{"plain torn tail is skipped", new("{\"elapsed_ms\":1000}\n{\"elapsed_ms\":20"), nil, false, []TrialConditions{{ElapsedMS: 1000}}},
		{"compressed torn tail is skipped", nil, new("{\"elapsed_ms\":1000}\n{\"elapsed_ms\":20"), false, []TrialConditions{{ElapsedMS: 1000}}},
		{"undecodable lines are skipped", nil, new("{\"elapsed_ms\":1000}\nnot json\n{\"elapsed_ms\":2000}\n"), false, both},
		{"corrupt compressed file yields nothing", nil, new("not gzip"), true, nil},
		{"neither form yields nothing", nil, nil, false, nil},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			if tt.plain != nil {
				if err := os.WriteFile(filepath.Join(dir, SamplesFile), []byte(*tt.plain), 0644); err != nil {
					t.Fatal(err)
				}
			}
			if tt.gz != nil {
				path := filepath.Join(dir, SamplesFile+CompressedSuffix)
				if tt.rawGz {
					if err := os.WriteFile(path, []byte(*tt.gz), 0644); err != nil {
						t.Fatal(err)
					}
				} else {
					writeSamplesGzip(t, path, *tt.gz)
				}
			}
			if diff := cmp.Diff(tt.want, slices.Collect(ReadSamples(dir))); diff != "" {
				t.Fatalf("samples (-want +got):\n%s", diff)
			}
		})
	}
}

func TestReadSamplesStopsEarly(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	writeSamplesGzip(t, filepath.Join(dir, SamplesFile+CompressedSuffix), "{\"elapsed_ms\":1}\n{\"elapsed_ms\":2}\n")
	var got []TrialConditions
	for s := range ReadSamples(dir) {
		got = append(got, s)
		break
	}
	if diff := cmp.Diff([]TrialConditions{{ElapsedMS: 1}}, got); diff != "" {
		t.Fatal(diff)
	}
}
