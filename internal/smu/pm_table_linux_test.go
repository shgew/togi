package smu

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func pmTableCores() []machine.CoreInfo {
	cores := make([]machine.CoreInfo, 16)
	for i := range cores {
		cores[i] = machine.CoreInfo{Core: i, CCD: i / 8}
	}
	return cores
}

func TestReadPMTable(t *testing.T) {
	t.Parallel()
	raw, want := syntheticPMTable()
	for _, tc := range []struct {
		name    string
		version []byte
		table   []byte
		decoded bool
	}{
		{"supported binary version", []byte{0x05, 0x02, 0x62, 0x00}, raw, true},
		{"missing files", nil, nil, false},
		{"missing table", []byte{0x05, 0x02, 0x62, 0x00}, nil, false},
		{"wrong version", []byte{0x05, 0x01, 0x62, 0x00}, raw, false},
		{"text version invalid", []byte("0x620205\n"), raw, false},
		{"short version invalid", []byte{0x05, 0x02, 0x62}, raw, false},
		{"long version invalid", []byte{0x05, 0x02, 0x62, 0x00, 0x00}, raw, false},
		{"short read", []byte{0x05, 0x02, 0x62, 0x00}, raw[:100], false},
		{"wrong size", []byte{0x05, 0x02, 0x62, 0x00}, append(slices.Clone(raw), 0), false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				files := map[string][]byte{"pm_table_version": tc.version, "pm_table": tc.table}
				c := NewConditions("/", pmTableCores(), func(path string) ([]byte, error) {
					if raw := files[filepath.Base(path)]; raw != nil {
						return raw, nil
					}
					return nil, os.ErrNotExist
				})
				synctest.Wait()
				got := c.PMTable()
				synctest.Wait()
				if tc.decoded {
					if diff := cmp.Diff(want, got); diff != "" {
						t.Fatal(diff)
					}
					delete(files, "pm_table")
					c.PMTable()
					synctest.Wait()
					if c.PMTable() != nil {
						t.Fatal("failed refresh retained old lanes")
					}
					synctest.Wait()
				} else if got != nil {
					t.Fatalf("unavailable table decoded: %+v", got)
				}
			})
		})
	}
}

func TestPMTableTopology(t *testing.T) {
	for _, tc := range []struct {
		name    string
		change  func([]machine.CoreInfo) []machine.CoreInfo
		decoded bool
	}{
		{"contiguous", func(c []machine.CoreInfo) []machine.CoreInfo { return c }, true},
		{"noncontiguous", func(c []machine.CoreInfo) []machine.CoreInfo {
			for i := 8; i < 16; i++ {
				c[i].Core += 8
			}
			return c
		}, false},
		{"reversed CCDs", func(c []machine.CoreInfo) []machine.CoreInfo {
			for i := range c {
				c[i].CCD = 1 - c[i].CCD
			}
			return c
		}, false},
		{"wrong slot order", func(c []machine.CoreInfo) []machine.CoreInfo { c[0], c[1] = c[1], c[0]; return c }, false},
		{"wrong core count", func(c []machine.CoreInfo) []machine.CoreInfo { return c[:8] }, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				raw, want := syntheticPMTable()
				var transfers atomic.Int64
				c := NewConditions("/", tc.change(pmTableCores()), func(path string) ([]byte, error) {
					if filepath.Base(path) == "pm_table_version" {
						return []byte{0x05, 0x02, 0x62, 0x00}, nil
					}
					transfers.Add(1)
					return raw, nil
				})
				check := c.Check()
				got := c.PMTable()
				synctest.Wait()
				if tc.decoded {
					if diff := cmp.Diff(want, got); diff != "" {
						t.Fatal(diff)
					}
				} else {
					if got != nil || transfers.Load() != 0 {
						t.Fatal("unsupported topology transferred or decoded lanes")
					}
					if !check.OK || !strings.Contains(check.Detail, "unsupported") {
						t.Fatalf("topology did not produce informational unavailability: %+v", check)
					}
				}
			})
		})
	}
}

func TestPMTableBlockingRead(t *testing.T) {
	for _, blockedFile := range []string{"pm_table_version", "pm_table"} {
		t.Run(blockedFile, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				raw, want := syntheticPMTable()
				release := make(chan struct{})
				var blockedReads atomic.Int64
				c := NewConditions("/", pmTableCores(), func(path string) ([]byte, error) {
					if filepath.Base(path) == blockedFile {
						blockedReads.Add(1)
						<-release
					}
					if filepath.Base(path) == "pm_table_version" {
						return []byte{0x05, 0x02, 0x62, 0x00}, nil
					}
					return raw, nil
				})
				synctest.Wait()
				started := time.Now()
				if c.PMTable() != nil || time.Since(started) != 0 {
					t.Error("sampler waited for pending lanes")
				}
				check := c.Check()
				if time.Since(started) != pmTableReadTimeout || !check.OK || !strings.Contains(check.Detail, "overdue") {
					t.Errorf("preflight exceeded read budget or lost unavailability: elapsed %s, %+v", time.Since(started), check)
				}
				if blockedFile == "pm_table" && !strings.Contains(check.Detail, "0x620205") {
					t.Error("preflight lost the version seen before the blocked transfer")
				}
				time.Sleep(10 * time.Second)
				started = time.Now()
				for range 20 {
					if c.PMTable() != nil {
						t.Error("overdue read supplied lanes")
					}
					c.Check()
				}
				if time.Since(started) != 0 || blockedReads.Load() != 1 {
					t.Error("overdue read delayed callers or spawned replacement readers")
				}
				close(release)
				synctest.Wait()
				if c.PMTable() != nil {
					t.Error("late completion was accepted")
				}
				synctest.Wait()
				if diff := cmp.Diff(want, c.PMTable()); diff != "" {
					t.Error(diff)
				}
				synctest.Wait()
			})
		})
	}
}

func TestPMTableFreshness(t *testing.T) {
	for _, age := range []time.Duration{time.Second, pmTableMaxAge, pmTableMaxAge + time.Nanosecond} {
		t.Run(age.String(), func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				raw, want := syntheticPMTable()
				release := make(chan struct{})
				reads := 0
				c := NewConditions("/", pmTableCores(), func(path string) ([]byte, error) {
					if filepath.Base(path) == "pm_table_version" {
						return []byte{0x05, 0x02, 0x62, 0x00}, nil
					}
					reads++
					if reads > 1 {
						<-release
					}
					return raw, nil
				})
				synctest.Wait()
				time.Sleep(age)
				got := c.PMTable()
				if age > pmTableMaxAge {
					want = nil
				}
				if diff := cmp.Diff(want, got); diff != "" {
					t.Error(diff)
				}
				synctest.Wait()
				time.Sleep(pmTableReadTimeout)
				if c.PMTable() != nil {
					t.Error("overdue refresh reused old lanes")
				}
				close(release)
				synctest.Wait()
			})
		})
	}
}
