package watchtest

import (
	"compress/gzip"
	"embed"
	"io"
	"os"
	"path/filepath"
	"testing"

	"github.com/shgew/togi/internal/journal"
)

//go:embed testdata/render-*.jsonl.gz
var fixtures embed.FS

func Install(t *testing.T, dir, name string) journal.Build {
	t.Helper()
	f, err := fixtures.Open("testdata/render-" + name + ".jsonl.gz")
	if err != nil {
		t.Fatalf("open render fixture: %v", err)
	}
	defer f.Close()
	r, err := gzip.NewReader(f)
	if err != nil {
		t.Fatalf("open render fixture gzip: %v", err)
	}
	defer r.Close()
	out, err := os.Create(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		t.Fatalf("create fixture journal: %v", err)
	}
	defer out.Close()
	if _, err := io.Copy(out, r); err != nil {
		t.Fatalf("install render fixture: %v", err)
	}
	build, _, err := journal.Scan(dir)
	if err != nil {
		t.Fatalf("scan fixture journal: %v", err)
	}
	return build
}
