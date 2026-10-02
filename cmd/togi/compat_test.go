package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
)

func incompatibleFixture(t *testing.T, field string) (string, []byte) {
	t.Helper()
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	stamp := `,"version":"0.2.1","rev":"def5678","ruleset":3,"schema":99`
	if field == "ruleset" {
		stamp = `,"version":"0.2.1","rev":"def5678","ruleset":99,"schema":2`
	}
	data := []byte(strings.Replace(string(fixture), `,"schema":2,"ruleset":3`, stamp, 1))
	if bytes.Equal(fixture, data) {
		t.Fatal("fixture did not contain schema stamp")
	}
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), data, 0o644); err != nil {
		t.Fatal(err)
	}
	return dir, data
}

func TestReadCommandsHandleIncompatibleJournal(t *testing.T) {
	for _, command := range []string{"status", "events"} {
		t.Run(command+" warns ruleset", func(t *testing.T) {
			dir, _ := incompatibleFixture(t, "ruleset")
			var stdout, stderr bytes.Buffer
			if code := cli([]string{"--state-dir", dir, command}, &stdout, &stderr); code != exitOK || !strings.Contains(stderr.String(), "ruleset 99") {
				t.Fatalf("%s exit %d, stdout %q, stderr %q", command, code, stdout.String(), stderr.String())
			}
		})
		t.Run(command+" refuses schema", func(t *testing.T) {
			dir, original := incompatibleFixture(t, "schema")
			var stdout, stderr bytes.Buffer
			args := []string{"--state-dir", dir, command}
			if command == "events" {
				args = append(args, "--json")
			}
			if code := cli(args, &stdout, &stderr); code != exitError || stdout.Len() != 0 || !strings.Contains(stderr.String(), "uses schema 2") {
				t.Fatalf("%s exit %d, stdout %q, stderr %q", command, code, stdout.String(), stderr.String())
			}
			after, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
			if err != nil || !bytes.Equal(after, original) {
				t.Fatalf("read changed journal: %v", err)
			}
		})
	}
}

func TestReadCommandsStyleSchemaRefusalInJournal(t *testing.T) {
	t.Setenv("NO_COLOR", "")
	for _, command := range []string{"status", "events"} {
		t.Run(command, func(t *testing.T) {
			dir, original := incompatibleFixture(t, "schema")
			stderr, err := os.CreateTemp(t.TempDir(), "refusal")
			if err != nil {
				t.Fatal(err)
			}
			defer stderr.Close()
			t.Setenv("JOURNAL_STREAM", journalStreamFor(t, stderr))
			var stdout bytes.Buffer
			if code := cli([]string{"--state-dir", dir, command}, &stdout, stderr); code != exitError {
				t.Fatalf("%s exit %d, want %d", command, code, exitError)
			}
			line, err := os.ReadFile(stderr.Name())
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.HasPrefix(line, []byte("<3>\x1b[1;31mtogi "+command+": this journal")) || !bytes.HasSuffix(line, []byte("\x1b[0m\n")) {
				t.Fatalf("schema refusal not red bold in journald: %q", line)
			}
			after, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
			if err != nil || !bytes.Equal(after, original) {
				t.Fatalf("read changed journal: %v", err)
			}
		})
	}
}

func TestResetAllArchivesUnrecognizedSchema(t *testing.T) {
	dir, original := incompatibleFixture(t, "schema")
	trial := filepath.Join(dir, "trials", "0001")
	if err := os.MkdirAll(trial, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(trial, "result"), []byte("evidence"), 0o644); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := testCLI(t, []string{"--state-dir", dir, "reset", "--all"}, &stdout, &stderr); code != exitOK || !strings.Contains(stdout.String(), "without appending") {
		t.Fatalf("reset exit %d, stdout %q, stderr %q", code, stdout.String(), stderr.String())
	}
	id := "20261002T011000Z"
	archived, err := os.ReadFile(filepath.Join(dir, "archive", id+".jsonl"))
	if err != nil || !bytes.Equal(archived, original) {
		t.Fatalf("archived journal changed: %v", err)
	}
	if proof, err := os.ReadFile(filepath.Join(dir, "archive", id+"-trials", "0001", "result")); err != nil || string(proof) != "evidence" {
		t.Fatalf("trial archive: %q, %v", proof, err)
	}
}

func TestResetCoreRefusesDifferentRuleset(t *testing.T) {
	dir, original := incompatibleFixture(t, "ruleset")
	var stdout, stderr bytes.Buffer
	if code := testCLI(t, []string{"--state-dir", dir, "reset", "--core", "3"}, &stdout, &stderr); code != exitError || !strings.Contains(stderr.String(), fmt.Sprintf("uses ruleset %d", session.Build().Ruleset)) {
		t.Fatalf("reset --core exit %d, stderr %q", code, stderr.String())
	}
	after, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil || !bytes.Equal(after, original) {
		t.Fatalf("reset --core changed journal: %v", err)
	}
}

func TestRunChecksCompatibilityBeforeConfig(t *testing.T) {
	dir, original := incompatibleFixture(t, "ruleset")
	configPath := filepath.Join(t.TempDir(), "invalid.toml")
	if err := os.WriteFile(configPath, []byte("removed_key = true\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "--config", configPath, "run"}, &stdout, &stderr); code != exitIncompatible || !strings.Contains(stderr.String(), "uses ruleset") || strings.Contains(stderr.String(), "removed_key") {
		t.Fatalf("run exit %d, stderr %q", code, stderr.String())
	}
	after, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil || !bytes.Equal(after, original) {
		t.Fatalf("run changed journal: %v", err)
	}
}

func TestRunStillRejectsInvalidConfigForCompatibleJournal(t *testing.T) {
	dir := t.TempDir()
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "events.jsonl")
	if err := os.WriteFile(path, fixture, 0o644); err != nil {
		t.Fatal(err)
	}
	configPath := filepath.Join(t.TempDir(), "invalid.toml")
	if err := os.WriteFile(configPath, []byte("removed_key = true\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "--config", configPath, "run"}, &stdout, &stderr); code != exitUsage {
		t.Fatalf("compatible journal: exit %d, want %d, stderr %q", code, exitUsage, stderr.String())
	}
	after, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(after, fixture) {
		t.Fatalf("config refusal changed journal: %v", err)
	}
}

func TestIncompatibleArchiveRefusesCollisionAndResumesPendingMove(t *testing.T) {
	dir, original := incompatibleFixture(t, "schema")
	id := "20261002T011000Z"
	archive := filepath.Join(dir, "archive")
	trialsArchive := filepath.Join(archive, id+"-trials")
	if err := os.MkdirAll(trialsArchive, 0o755); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	args := []string{"--state-dir", dir, "reset", "--all"}
	if code := testCLI(t, args, &stdout, &stderr); code != exitError || !strings.Contains(stderr.String(), "already exists") {
		t.Fatalf("collision exit %d: %s", code, stderr.String())
	}
	after, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil || !bytes.Equal(after, original) {
		t.Fatalf("collision changed journal: %v", err)
	}
	if err := os.WriteFile(filepath.Join(archive, id+"-compat-pending"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	stdout.Reset()
	stderr.Reset()
	if code := testCLI(t, args, &stdout, &stderr); code != exitOK {
		t.Fatalf("resume archive exit %d: %s", code, stderr.String())
	}
	archived, err := os.ReadFile(filepath.Join(archive, id+".jsonl"))
	if err != nil || !bytes.Equal(archived, original) {
		t.Fatalf("recovered archive: %v", err)
	}
	if _, err := os.Stat(filepath.Join(archive, id+"-compat-pending")); !os.IsNotExist(err) {
		t.Fatalf("pending marker remains: %v", err)
	}
}

func TestResetAllCompletesRenamedIncompatibleArchive(t *testing.T) {
	dir, original := incompatibleFixture(t, "schema")
	id := "20261002T011000Z"
	archive := filepath.Join(dir, "archive")
	if err := os.Mkdir(archive, 0o755); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(archive, id+".jsonl")
	if err := os.Rename(filepath.Join(dir, "events.jsonl"), path); err != nil {
		t.Fatal(err)
	}
	marker := filepath.Join(archive, id+"-compat-pending")
	if err := os.WriteFile(marker, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := testCLI(t, []string{"--state-dir", dir, "reset", "--all"}, &stdout, &stderr); code != exitOK || !strings.Contains(stdout.String(), "session "+id+" archived to archive/") {
		t.Fatalf("resume archive exit %d, stdout %q, stderr %q", code, stdout.String(), stderr.String())
	}
	if _, err := os.Stat(marker); !os.IsNotExist(err) {
		t.Fatalf("pending marker remains: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "events.jsonl")); !os.IsNotExist(err) {
		t.Fatalf("reset recreated journal: %v", err)
	}
	archived, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(archived, original) {
		t.Fatalf("archived journal changed: %v", err)
	}
}

func TestResetAllArchivesDifferentRulesetNormally(t *testing.T) {
	dir, original := incompatibleFixture(t, "ruleset")
	var stdout, stderr bytes.Buffer
	if code := testCLI(t, []string{"--state-dir", dir, "reset", "--all"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("reset exit %d: %s", code, stderr.String())
	}
	path := filepath.Join(dir, "archive", "20261002T011000Z.jsonl")
	archived, err := os.ReadFile(path)
	if err != nil || !bytes.HasPrefix(archived, original) || !bytes.Contains(archived, []byte(`"kind":"session.archived"`)) {
		t.Fatalf("normal archive: %v, %q", err, archived)
	}
}

func TestResetAllCompletesRecordedOldSchemaArchive(t *testing.T) {
	dir, data := incompatibleFixture(t, "schema")
	id := "20261002T011000Z"
	path := filepath.Join(dir, "events.jsonl")
	recorded := []byte(`{"seq":12,"time":"2026-10-02T01:20:01Z","boot":"old","kind":"session.archived","session":"` + id + `","path":"archive/` + id + `.jsonl"}` + "\n")
	data = append(data, recorded...)
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(dir, "archive", id+"-trials"), 0o755); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := testCLI(t, []string{"--state-dir", dir, "reset", "--all"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("finish archive exit %d: %s", code, stderr.String())
	}
	archived, err := os.ReadFile(filepath.Join(dir, "archive", id+".jsonl"))
	if err != nil || !bytes.Equal(archived, data) {
		t.Fatalf("recorded archive changed: %v", err)
	}
}

func TestUnknownKindsBeforeRunConfigAndReset(t *testing.T) {
	for _, tc := range []struct {
		name    string
		ruleset int
		schema  int
	}{
		{"older ruleset", session.Build().Ruleset - 1, journal.Schema},
		{"older schema", session.Build().Ruleset, journal.Schema - 1},
		{"current build", session.Build().Ruleset, journal.Schema},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			path := filepath.Join(dir, "events.jsonl")
			original := fmt.Appendf(nil, "{\"seq\":1,\"time\":\"2026-09-01T00:00:00Z\",\"boot\":\"old\",\"kind\":\"session.start\",\"session\":\"20260901T000000Z\",\"ruleset\":%d,\"schema\":%d}\n{\"seq\":2,\"time\":\"2026-09-01T00:00:01Z\",\"boot\":\"old\",\"kind\":\"retired.fact\",\"msg\":\"unknown fact\"}\n", tc.ruleset, tc.schema)
			if err := os.WriteFile(path, original, 0o644); err != nil {
				t.Fatal(err)
			}
			configPath := filepath.Join(dir, "invalid.toml")
			if err := os.WriteFile(configPath, []byte("removed_key = true\n"), 0o644); err != nil {
				t.Fatal(err)
			}
			var stdout, stderr bytes.Buffer
			wantCode := exitUsage
			wantError := "removed_key"
			if tc.ruleset == session.Build().Ruleset && tc.schema == journal.Schema {
				wantCode = exitIncompatible
				wantError = `unknown kind "retired.fact"`
			}
			if code := testCLI(t, []string{"--state-dir", dir, "--config", configPath, "run"}, &stdout, &stderr); code != wantCode || !strings.Contains(stderr.String(), wantError) {
				t.Fatalf("run exit %d, want %d; stderr %q", code, wantCode, stderr.String())
			}
			after, err := os.ReadFile(path)
			if err != nil || !bytes.Equal(original, after) {
				t.Fatalf("run refusal changed journal: %v", err)
			}
			stdout.Reset()
			stderr.Reset()
			code := testCLI(t, []string{"--state-dir", dir, "reset", "--all"}, &stdout, &stderr)
			if tc.schema != journal.Schema {
				if code != exitOK {
					t.Fatalf("schema reset exit %d: %s", code, &stderr)
				}
				archived, err := os.ReadFile(filepath.Join(dir, "archive", "20260901T000000Z.jsonl"))
				if err != nil || !bytes.Equal(original, archived) {
					t.Fatalf("schema reset changed archive: %v", err)
				}
			} else {
				if code != exitError || !strings.Contains(stderr.String(), `unknown kind "retired.fact"`) {
					t.Fatalf("same-schema reset exit %d: %s", code, &stderr)
				}
				after, err := os.ReadFile(path)
				if err != nil || !bytes.Equal(original, after) {
					t.Fatalf("same-schema reset changed journal: %v", err)
				}
			}
		})
	}
}

func TestCommandsWithFutureKind(t *testing.T) {
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	future, err := os.ReadFile("testdata/future-kind.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	data := append(bytes.Clone(fixture), future...)
	data = bytes.Replace(data, []byte(`"ruleset":3`), fmt.Appendf(nil, `"ruleset":%d`, session.Build().Ruleset), 1)
	for _, tc := range []struct {
		args []string
		code int
	}{
		{[]string{"status"}, exitOK},
		{[]string{"events"}, exitOK},
		{[]string{"events", "--json"}, exitOK},
		{[]string{"watch", "--width", "120", "--height", "33"}, exitOK},
		{[]string{"run"}, exitIncompatible},
		{[]string{"reset", "--core", "3"}, exitError},
		{[]string{"reset", "--all"}, exitError},
	} {
		t.Run(strings.Join(tc.args, " "), func(t *testing.T) {
			dir := t.TempDir()
			path := filepath.Join(dir, "events.jsonl")
			if err := os.WriteFile(path, data, 0o644); err != nil {
				t.Fatal(err)
			}
			var stdout, stderr bytes.Buffer
			code := testCLI(t, append([]string{"--state-dir", dir}, tc.args...), &stdout, &stderr)
			if diff := cmp.Diff(tc.code, code); diff != "" {
				t.Fatalf("exit (-want +got): %s; stderr %s", diff, stderr.String())
			}
			if tc.code != exitOK {
				events, _, err := journal.Read(dir)
				if err != nil {
					t.Fatal(err)
				}
				want := (&journal.UnknownKindError{Kind: "future.fact", Journal: journal.BuildOf(events), Binary: session.Build()}).Error()
				if !strings.Contains(stderr.String(), want) {
					t.Fatalf("missing refusal: %s", stderr.String())
				}
			} else if tc.args[0] == "events" {
				want := "future fact remains visible"
				if len(tc.args) > 1 {
					want = string(future)
				}
				if !strings.Contains(stdout.String(), want) {
					t.Fatalf("missing future event: %s", stdout.String())
				}
			}
			after, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(data, after); diff != "" {
				t.Fatalf("journal changed (-want +got): %s", diff)
			}
		})
	}
}
