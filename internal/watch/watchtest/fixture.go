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

func Install(tb testing.TB, dir, name string) journal.Build {
	tb.Helper()
	f, err := fixtures.Open("testdata/render-" + name + ".jsonl.gz")
	if err != nil {
		tb.Fatalf("open render fixture: %v", err)
	}
	defer f.Close()
	r, err := gzip.NewReader(f)
	if err != nil {
		tb.Fatalf("open render fixture gzip: %v", err)
	}
	defer r.Close()
	out, err := os.Create(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		tb.Fatalf("create fixture journal: %v", err)
	}
	defer out.Close()
	if _, err := io.Copy(out, r); err != nil {
		tb.Fatalf("install render fixture: %v", err)
	}
	build, _, err := journal.Scan(dir)
	if err != nil {
		tb.Fatalf("scan fixture journal: %v", err)
	}
	return build
}
