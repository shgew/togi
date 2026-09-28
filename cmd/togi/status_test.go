package main

import (
	"bytes"
	"context"
	"crypto/sha256"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"testing"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
)

func TestStatusAndCert(t *testing.T) {
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	simulated(t, dir)
	st, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	raw, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}

	stdout.Reset()
	if code := cli([]string{"--state-dir", dir, "status"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("status: exit %d, stderr %s", code, stderr.String())
	}
	status := stdout.String()
	if want := fmt.Sprintf("tier bronze [#%d]", st.TierSeq); !strings.Contains(status, want) {
		t.Fatalf("status lacks %q:\n%s", want, status)
	}
	checkRows(t, "status", status, regexp.MustCompile(`(?m)^(\d\d)  +\d  +(-?\d+)  `), st)
	golden(t, "status", status)

	stdout.Reset()
	if code := cli([]string{"--state-dir", dir, "cert"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("cert: exit %d, stderr %s", code, stderr.String())
	}
	cert := stdout.String()
	if want := regexp.MustCompile(fmt.Sprintf(`BRONZE +\[tier\.change #%d\]`, st.TierSeq)); !want.MatchString(cert) {
		t.Fatalf("cert lacks %s:\n%s", want, cert)
	}
	for _, want := range []string{
		"Platinum  locked until togi observe exists",
		fmt.Sprintf("Journal SHA-256 %x through seq %d", sha256.Sum256(raw), st.LastSeq),
	} {
		if !strings.Contains(cert, want) {
			t.Fatalf("cert lacks %q:\n%s", want, cert)
		}
	}
	checkRows(t, "cert", cert, regexp.MustCompile(`(?m)^  (\d\d)  +(-?\d+)  `), st)
	golden(t, "cert", strings.ReplaceAll(cert, fmt.Sprintf("%x", sha256.Sum256(raw)), "<journal sha256>"))

	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	moved := st
	moved.Cores = append([]journal.CoreState(nil), st.Cores...)
	moved.Cores[3].Offset++
	var out bytes.Buffer
	writeCert(&out, events, moved)
	checkRows(t, "cert with core 03 moved", out.String(), regexp.MustCompile(`(?m)^  (\d\d)  +(-?\d+)  `), st)
	if want := fmt.Sprintf("core 03 is at %d since", moved.Cores[3].Offset); !strings.Contains(out.String(), want) {
		t.Fatalf("cert with core 03 moved lacks %q:\n%s", want, out.String())
	}
}

func TestStatusShowsUnresetDefectResetCommands(t *testing.T) {
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	simulated(t, dir)
	record := func(payload journal.Payload) {
		t.Helper()
		j, err := journal.Open(dir, journal.Options{Boot: "status-test", Build: session.Build()})
		if err != nil {
			t.Fatal(err)
		}
		if _, err := j.Append(payload); err != nil {
			t.Fatal(err)
		}
		if err := j.Close(); err != nil {
			t.Fatal(err)
		}
	}
	status := func() string {
		t.Helper()
		stdout.Reset()
		stderr.Reset()
		if code := cli([]string{"--state-dir", dir, "status"}, &stdout, &stderr); code != exitOK {
			t.Fatalf("status: exit %d, stderr %s", code, stderr.String())
		}
		return stdout.String()
	}
	check := func(want []string, absent []string) {
		t.Helper()
		out := status()
		for _, text := range want {
			if !strings.Contains(out, text) {
				t.Fatalf("status lacks %q:\n%s", text, out)
			}
		}
		for _, text := range absent {
			if strings.Contains(out, text) {
				t.Fatalf("status unexpectedly shows %q:\n%s", text, out)
			}
		}
	}
	title := "defect 1: False failure at power-off"
	core3, core7 := "togi reset --core 3", "togi reset --core 7"
	record(&journal.DefectFound{ID: 1, Title: "False failure at power-off", PR: 16, Direction: "too_cautious", Cores: []int{3, 7}, Decisions: []int{42}})
	check([]string{title, core3, core7}, nil)
	record(&journal.DefectAnswered{ID: 1, Cores: []int{3, 7}, Answer: "no"})
	check([]string{title, core3, core7}, nil)
	record(&journal.CommandReset{Core: new(3)})
	check([]string{title, core7, "affected cores [7]"}, []string{core3})
	record(&journal.CommandReset{Core: new(7)})
	check(nil, []string{title, core3, core7})
	record(&journal.DefectFound{ID: 1, Title: "False failure at power-off", PR: 16, Direction: "too_cautious", Cores: []int{3, 7}, Decisions: []int{42}})
	record(&journal.CommandReset{All: true})
	check(nil, []string{title, core3, core7})
}

func TestRateRoundsUp(t *testing.T) {
	t.Parallel()
	for bound, want := range map[float64]string{0.1241: "< 0.13/h", 0.12: "< 0.12/h", 3: "< 3.00/h"} {
		if got := rate(&bound); got != want {
			t.Errorf("rate(%v) = %q, want %q", bound, got, want)
		}
	}
}

var (
	simulatedOnce  sync.Once
	simulatedFiles map[string][]byte
	simulatedErr   error
)

func simulated(t *testing.T, dir string) {
	t.Helper()
	simulatedOnce.Do(func() {
		src := t.TempDir()
		m, err := sim.New(sim.Config{Seed: 1})
		if err != nil {
			simulatedErr = err
			return
		}
		stop, err := simrun.Simulate(context.Background(), simrun.Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: src, Machine: m, Rotations: 2})
		if err != nil || stop.Reason != session.StopRotations {
			simulatedErr = fmt.Errorf("simulate: %+v, %w", stop, err)
			return
		}
		simulatedFiles = make(map[string][]byte)
		simulatedErr = filepath.WalkDir(src, func(path string, d fs.DirEntry, err error) error {
			if err != nil || !d.Type().IsRegular() {
				return err
			}
			data, err := os.ReadFile(path)
			if err != nil {
				return err
			}
			rel, err := filepath.Rel(src, path)
			if err != nil {
				return err
			}
			simulatedFiles[rel] = data
			return nil
		})
	})
	if simulatedErr != nil {
		t.Fatal(simulatedErr)
	}
	for rel, data := range simulatedFiles {
		path := filepath.Join(dir, rel)
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, data, 0o644); err != nil {
			t.Fatal(err)
		}
	}
}

func checkRows(t *testing.T, name, out string, row *regexp.Regexp, st journal.State) {
	t.Helper()
	rows := row.FindAllStringSubmatch(out, -1)
	if len(rows) != len(st.Cores) || len(rows) != 16 {
		t.Fatalf("%s: %d core rows, want 16:\n%s", name, len(rows), out)
	}
	for i, r := range rows {
		core, _ := strconv.Atoi(r[1])
		offset, _ := strconv.Atoi(r[2])
		if c := st.Cores[i]; core != c.Core || offset != c.Offset {
			t.Errorf("%s row %d: core %d offset %d, want core %d offset %d", name, i, core, offset, c.Core, c.Offset)
		}
	}
}
