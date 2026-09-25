package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func incompatibleFixture(t *testing.T, field string) (string, []byte) {
	t.Helper()
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	stamp := `,"version":"0.2.1","rev":"def5678","ruleset":1,"schema":99`
	if field == "ruleset" {
		stamp = `,"version":"0.2.1","rev":"def5678","ruleset":99,"schema":1`
	}
	data := []byte(strings.Replace(string(fixture), `,"schema":1`, stamp, 1))
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
	for _, command := range []string{"status", "cert", "events"} {
		t.Run(command+" warns ruleset", func(t *testing.T) {
			dir, _ := incompatibleFixture(t, "ruleset")
			var stdout, stderr bytes.Buffer
			if code := cli([]string{"--state-dir", dir, command}, &stdout, &stderr); code != exitOK || !strings.Contains(stderr.String(), "warning: journal written by shycler 0.2.1+def5678") || stdout.Len() == 0 {
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
			if code := cli(args, &stdout, &stderr); code != exitError || stdout.Len() != 0 || !strings.Contains(stderr.String(), "uses schema 1") {
				t.Fatalf("%s exit %d, stdout %q, stderr %q", command, code, stdout.String(), stderr.String())
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
	if code := cli([]string{"--state-dir", dir, "reset", "--all"}, &stdout, &stderr); code != exitOK || !strings.Contains(stdout.String(), "without appending") {
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

func TestRegainRefusesDifferentRuleset(t *testing.T) {
	dir, original := incompatibleFixture(t, "ruleset")
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "regain"}, &stdout, &stderr); code != exitError || !strings.Contains(stderr.String(), "uses ruleset 1") {
		t.Fatalf("regain exit %d, stderr %q", code, stderr.String())
	}
	after, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil || !bytes.Equal(after, original) {
		t.Fatalf("regain changed journal: %v", err)
	}
}

func TestSimulatedRunRefusesIncompatibleJournal(t *testing.T) {
	dir, original := incompatibleFixture(t, "ruleset")
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "run", "--sim", "1"}, &stdout, &stderr); code != exitIncompatible || !strings.Contains(stderr.String(), "Install shycler 0.2.1") {
		t.Fatalf("run exit %d, stderr %q", code, stderr.String())
	}
	after, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil || !bytes.Equal(after, original) {
		t.Fatalf("run changed journal: %v", err)
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
	if code := cli(args, &stdout, &stderr); code != exitError || !strings.Contains(stderr.String(), "already exists") {
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
	if code := cli(args, &stdout, &stderr); code != exitOK {
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

func TestResetAllArchivesDifferentRulesetNormally(t *testing.T) {
	dir, original := incompatibleFixture(t, "ruleset")
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "reset", "--all"}, &stdout, &stderr); code != exitOK {
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
	if code := cli([]string{"--state-dir", dir, "reset", "--all"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("finish archive exit %d: %s", code, stderr.String())
	}
	archived, err := os.ReadFile(filepath.Join(dir, "archive", id+".jsonl"))
	if err != nil || !bytes.Equal(archived, data) {
		t.Fatalf("recorded archive changed: %v", err)
	}
}
