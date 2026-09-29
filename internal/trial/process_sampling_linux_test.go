//go:build linux

package trial

import (
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

func statFixture(user, system, cpu string) string {
	fields := strings.Fields("R " + strings.Repeat("0 ", 36))
	fields[14-3], fields[15-3], fields[39-3] = user, system, cpu
	return "123 (backend (worker)) " + strings.Join(fields, " ") + "\n"
}

func writeStatFixture(t *testing.T, path, data string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(data), 0644); err != nil {
		t.Fatal(err)
	}
}

func TestProcUsageValidity(t *testing.T) {
	for _, tc := range []struct {
		name, data string
		want       usage
		loss       bool
	}{
		{"valid", statFixture("125", "25", "7"), usage{CPUTime: 1500 * time.Millisecond, CPU: 7}, false},
		{"missing", "", usage{}, false},
		{"short", "123 (backend) R 0", usage{}, true},
		{"malformed", "not a stat", usage{}, true},
		{"user", statFixture("bad", "0", "0"), usage{}, true},
		{"system", statFixture("0", "bad", "0"), usage{}, true},
		{"processor", statFixture("0", "0", "bad"), usage{}, true},
		{"negative", statFixture("-1", "0", "0"), usage{}, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			path := filepath.Join(t.TempDir(), "stat")
			if tc.data != "" {
				writeStatFixture(t, path, tc.data)
			}
			got, err := procUsage(path)
			if tc.loss {
				if err == nil || errors.Is(err, os.ErrNotExist) {
					t.Fatalf("malformed usage not reported as loss: %+v, %v", got, err)
				}
				return
			}
			if tc.name == "missing" {
				if !errors.Is(err, os.ErrNotExist) {
					t.Fatalf("disappearance not preserved: %v", err)
				}
			} else if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestProcThreadsValidity(t *testing.T) {
	for _, tc := range []struct {
		name, broken string
		loss         bool
	}{
		{"exited", "", false},
		{"malformed", "not a stat", true},
		{"processor", statFixture("0", "0", "bad"), true},
		{"unreadable", "directory", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			path := t.TempDir()
			writeStatFixture(t, filepath.Join(path, "123", "stat"), statFixture("0", "0", "7"))
			broken := filepath.Join(path, "124", "stat")
			if tc.broken == "directory" {
				if err := os.MkdirAll(broken, 0755); err != nil {
					t.Fatal(err)
				}
			} else {
				writeStatFixture(t, broken, tc.broken)
				if tc.broken == "" {
					if err := os.Remove(broken); err != nil {
						t.Fatal(err)
					}
				}
			}
			got, err := procThreads(path)
			if diff := cmp.Diff([]thread{{TID: 123, CPU: 7}}, got); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff(tc.loss, err != nil); diff != "" {
				t.Fatal(diff)
			}
			if errors.Is(err, os.ErrNotExist) {
				t.Fatalf("read failure confused with disappearance: %v", err)
			}
		})
	}
}

func TestProcThreadsMalformedID(t *testing.T) {
	path := t.TempDir()
	for _, name := range []string{"bad", "-1", "0"} {
		writeStatFixture(t, filepath.Join(path, name, "stat"), statFixture("0", "0", "7"))
	}
	writeStatFixture(t, filepath.Join(path, strconv.Itoa(123), "stat"), statFixture("0", "0", "7"))
	got, err := procThreads(path)
	if diff := cmp.Diff([]thread{{TID: 123, CPU: 7}}, got); diff != "" {
		t.Fatal(diff)
	}
	if err == nil || errors.Is(err, os.ErrNotExist) {
		t.Fatalf("malformed thread IDs not reported as loss: %v", err)
	}
}
