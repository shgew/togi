package sim

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestResumeAfterIncompatibleArchive(t *testing.T) {
	dir := t.TempDir()
	archive := filepath.Join(dir, "archive")
	if err := os.Mkdir(archive, 0o755); err != nil {
		t.Fatal(err)
	}
	data := []byte(`{"seq":1,"time":"2026-10-02T01:14:07Z","boot":"old","kind":"session.start","session":"old","schema":99}` + "\n" +
		`{"seq":2,"time":"2026-10-02T01:14:08Z","boot":"old","kind":"later.unknown"}` + "\n")
	if err := os.WriteFile(filepath.Join(archive, "old.jsonl"), data, 0o644); err != nil {
		t.Fatal(err)
	}
	got, err := Resume(dir, Config{Seed: 1})
	want := time.Date(2026, 10, 2, 1, 14, 8, 0, time.UTC).Add(RebootTime)
	if err != nil || got.Boots != 1 || !got.Start.Equal(want) {
		t.Fatalf("resume archived schema: %+v, %v; want one boot and start %s", got, err, want)
	}
}

func TestResumeContextPrecedence(t *testing.T) {
	for _, schema := range []int{journal.Schema, 99} {
		for _, tc := range []struct {
			name       string
			current    string
			configured string
			want       string
		}{
			{name: "latest numeric archive", want: "latest"},
			{name: "current journal", current: "current", want: "current"},
			{name: "configured context", configured: "configured", want: "configured"},
			{name: "configured over current", current: "current", configured: "configured", want: "configured"},
		} {
			t.Run(fmt.Sprintf("schema-%d/%s", schema, tc.name), func(t *testing.T) {
				dir := t.TempDir()
				archive := filepath.Join(dir, "archive")
				if err := os.Mkdir(archive, 0o755); err != nil {
					t.Fatal(err)
				}
				now := time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC)
				writeContext := func(path, id, version string, schema int) {
					t.Helper()
					scratch := t.TempDir()
					j, err := journal.Open(scratch, journal.Options{Boot: id, Now: func() time.Time { return now }})
					if err != nil {
						t.Fatal(err)
					}
					for _, payload := range []journal.Payload{
						&journal.SessionStart{Schema: schema, Session: id},
						&journal.SessionContext{BIOSVersion: version},
					} {
						if _, err := j.Append(payload); err != nil {
							_ = j.Close()
							t.Fatal(err)
						}
					}
					if err := j.Close(); err != nil {
						t.Fatal(err)
					}
					if err := os.Rename(filepath.Join(scratch, "events.jsonl"), path); err != nil {
						t.Fatal(err)
					}
				}
				for _, archived := range []struct {
					id      string
					version string
				}{
					{"20261002T011407Z", "oldest"},
					{"20261002T011407Z-2", "middle"},
					{"20261002T011407Z-10", "latest"},
				} {
					writeContext(filepath.Join(archive, archived.id+".jsonl"), archived.id, archived.version, schema)
				}
				if tc.current != "" {
					writeContext(filepath.Join(dir, "events.jsonl"), "current", tc.current, journal.Schema)
				}
				cfg := Config{}
				if tc.configured != "" {
					cfg.BIOSContext = machine.BIOSContext{BIOSVersion: tc.configured}
				}
				got, err := Resume(dir, cfg)
				if err != nil {
					t.Fatal(err)
				}
				if diff := cmp.Diff(machine.BIOSContext{BIOSVersion: tc.want}, got.BIOSContext); diff != "" {
					t.Fatalf("resumed BIOS context (-want +got):\n%s", diff)
				}
			})
		}
	}
}

func TestResumeEmptyAndCorruptHistory(t *testing.T) {
	cfg := Config{Cores: 2, Seed: 42, Boots: 7, Start: epoch.Add(time.Hour)}
	got, err := Resume(t.TempDir(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(cfg, got); diff != "" {
		t.Fatal(diff)
	}
	for _, tc := range []struct{ name, data, want string }{
		{"current corrupt", "not-json\n", "resume simulator"},
		{"archive metadata corrupt", "{\"seq\":1,\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"old\",\"kind\":\"session.start\",\"schema\":99}\nnot-json\n", "metadata"},
		{"archive context corrupt", "{\"seq\":1,\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"old\",\"kind\":\"session.start\",\"schema\":99}\n{\"seq\":2,\"time\":\"2026-01-01T00:00:01Z\",\"boot\":\"old\",\"kind\":\"session.context\",\"bios_version\":2}\n", "context"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(tc.data), 0600); err != nil {
				t.Fatal(err)
			}
			if _, err := Resume(dir, Config{}); err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("resume error = %v, want %q", err, tc.want)
			}
		})
	}
}

func TestResumeIgnoresTornIncompatibleTail(t *testing.T) {
	dir := t.TempDir()
	data := "{\"seq\":1,\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"old\",\"kind\":\"session.start\",\"schema\":99}\n{\"boot\":\"torn\",\"time\":\"2027-01-01T00:00:00Z\"}"
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(data), 0600); err != nil {
		t.Fatal(err)
	}
	got, err := Resume(dir, Config{})
	if err != nil || got.Boots != 1 || !got.Start.Equal(epoch.Add(RebootTime)) {
		t.Fatalf("torn resume = %+v, %v", got, err)
	}
}

func TestResumeReadsArchivesOfPatternLikeStateDir(t *testing.T) {
	now := time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC)
	for _, name := range []string{"state", "state[", "state[1]"} {
		t.Run(name, func(t *testing.T) {
			dir := filepath.Join(t.TempDir(), name)
			if err := os.Mkdir(dir, 0o755); err != nil {
				t.Fatal(err)
			}
			cfg := Config{Seed: 1, Boots: 7, Start: epoch}
			got, err := Resume(dir, cfg)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(cfg, got); diff != "" {
				t.Fatalf("resume without an archive (-want +got):\n%s", diff)
			}
			if err := os.Mkdir(filepath.Join(dir, "archive"), 0o755); err != nil {
				t.Fatal(err)
			}
			scratch := t.TempDir()
			j, err := journal.Open(scratch, journal.Options{Boot: "archived", Now: func() time.Time { return now }})
			if err != nil {
				t.Fatal(err)
			}
			for _, payload := range []journal.Payload{
				&journal.SessionStart{Schema: journal.Schema, Session: "archived"},
				&journal.SessionContext{BIOSContext: machine.BIOSContext{BIOSVersion: "archived"}},
			} {
				if _, err := j.Append(payload); err != nil {
					_ = j.Close()
					t.Fatal(err)
				}
			}
			if err := j.Close(); err != nil {
				t.Fatal(err)
			}
			if err := os.Rename(filepath.Join(scratch, "events.jsonl"), filepath.Join(dir, "archive", "archived.jsonl")); err != nil {
				t.Fatal(err)
			}
			got, err = Resume(dir, Config{Seed: 1})
			if err != nil {
				t.Fatal(err)
			}
			want := Config{Seed: 1, Boots: 1, Start: now.Add(RebootTime), BIOSContext: machine.BIOSContext{BIOSVersion: "archived"}}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("resume after archived session (-want +got):\n%s", diff)
			}
		})
	}
}
