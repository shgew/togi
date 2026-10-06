package hardware

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/backend/mprime"
	"github.com/shgew/togi/internal/backend/ycruncher"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/smu"
	"github.com/shgew/togi/internal/trial"
)

func TestRanking(t *testing.T) {
	root := t.TempDir()
	cores := []machine.CoreInfo{{Core: 0, CPUs: []int{4, 20}}, {Core: 8, CPUs: []int{8, 24}}}
	for _, row := range []struct{ cpu, value string }{{"4", "125\n"}, {"8", "174\n"}} {
		path := filepath.Join(root, "sys/devices/system/cpu/cpufreq/policy"+row.cpu)
		if err := os.MkdirAll(path, 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(path, "amd_pstate_prefcore_ranking"), []byte(row.value), 0644); err != nil {
			t.Fatal(err)
		}
	}
	got, err := ranking(root, cores)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]int{125, 174}, got); diff != "" {
		t.Fatalf("ranking (-want +got):\n%s", diff)
	}
	if err := os.Remove(filepath.Join(root, "sys/devices/system/cpu/cpufreq/policy8/amd_pstate_prefcore_ranking")); err != nil {
		t.Fatal(err)
	}
	if _, err := ranking(root, cores); !errors.Is(err, fs.ErrNotExist) || !strings.Contains(err.Error(), "read preferred-core ranking of cpu 8:") {
		t.Fatalf("missing ranking error = %v", err)
	}
}

func TestClockCancellation(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := (clock{}).Sleep(ctx, time.Hour); !errors.Is(err, context.Canceled) {
		t.Fatalf("sleep cancellation = %v, want %v", err, context.Canceled)
	}
}

func TestClockSleepDeadline(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		start := time.Now()
		if err := (clock{}).Sleep(context.Background(), time.Hour); err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(time.Hour, time.Since(start)); diff != "" {
			t.Fatalf("sleep duration (-want +got): %s", diff)
		}
	})
}

func TestRankingInvalidValue(t *testing.T) {
	root := t.TempDir()
	dir := filepath.Join(root, "sys/devices/system/cpu/cpufreq/policy4")
	if err := os.MkdirAll(dir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "amd_pstate_prefcore_ranking"), []byte("not-a-rank\n"), 0644); err != nil {
		t.Fatal(err)
	}
	values, err := ranking(root, []machine.CoreInfo{{Core: 9, CPUs: []int{4, 20}}})
	if values != nil || err == nil || !strings.Contains(err.Error(), "read preferred-core ranking of cpu 4:") || !strings.Contains(err.Error(), "not-a-rank") {
		t.Fatalf("invalid ranking: values=%v err=%v", values, err)
	}
}

func TestBackendUserPreflight(t *testing.T) {
	for _, tt := range []struct {
		name    string
		account string
		user    trial.Identity
		err     error
		want    machine.Check
	}{
		{"resolved", "trial-user", trial.Identity{UID: 123, GID: 456}, nil, machine.Check{Name: "backend_user", Detail: "trial-user: uid 123 gid 456", OK: true}},
		{"refused", "", trial.Identity{}, errors.New("backend_user not configured"), machine.Check{Name: "backend_user", Detail: "backend_user not configured"}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := host{cfg: config.Config{BackendUser: tt.account}, user: tt.user, userErr: tt.err}
			if diff := cmp.Diff(tt.want, h.checkBackendUser()); diff != "" {
				t.Fatalf("backend user check (-want +got):\n%s", diff)
			}
		})
	}
}

func TestBackendPreflight(t *testing.T) {
	for _, tt := range []struct {
		name              string
		mprime, ycruncher bool
		executable        bool
		wantOK            bool
		wantDetails       []string
	}{
		{"unconfigured", false, false, true, false, []string{"backends.mprime not configured", "backends.ycruncher not configured"}},
		{"missing y-cruncher", true, false, true, false, []string{"mprime: ", "backends.ycruncher not configured"}},
		{"missing mprime", false, true, true, false, []string{"backends.mprime not configured", "y-cruncher: "}},
		{"nonexecutable", true, true, false, false, []string{"mprime: ", "not executable; y-cruncher: ", "not executable"}},
		{"ready", true, true, true, true, []string{"mprime: ", "; y-cruncher: ", "00-x64 ~ baseline", "24-ZN5 ~ Zen 5"}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			root := t.TempDir()
			mode := os.FileMode(0644)
			if tt.executable {
				mode = 0755
			}
			for _, rel := range []string{"bin/mprime", "lib/y-cruncher/Binaries/00-x64 ~ baseline", "lib/y-cruncher/Binaries/24-ZN5 ~ Zen 5"} {
				path := filepath.Join(root, rel)
				if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(path, nil, mode); err != nil {
					t.Fatal(err)
				}
			}
			h := host{backends: map[machine.Backend]backend.Backend{}}
			if tt.mprime {
				h.backends[machine.Mprime] = mprime.New(root)
			}
			if tt.ycruncher {
				h.backends[machine.Ycruncher] = ycruncher.New(root)
			}
			got := h.checkBackends()
			if diff := cmp.Diff(machine.Check{Name: "backends", OK: tt.wantOK}, machine.Check{Name: got.Name, OK: got.OK}); diff != "" {
				t.Fatalf("backend readiness (-want +got):\n%s", diff)
			}
			for _, detail := range tt.wantDetails {
				if !strings.Contains(got.Detail, detail) {
					t.Fatalf("backend detail %q missing %q", got.Detail, detail)
				}
			}
		})
	}
}

type preflightMailbox struct{}

func (preflightMailbox) Command(uint32, [6]uint32) ([6]uint32, error) {
	return [6]uint32{}, errors.New("mailbox unavailable")
}

func (preflightMailbox) ReadSMN(uint32) (uint32, error) {
	return 0, errors.New("mailbox unavailable")
}

func TestPreflightRefusesUnvalidatedHardware(t *testing.T) {
	for _, tt := range []struct {
		name          string
		family        int
		driverPresent bool
		missingCPU    bool
	}{
		{"unsupported CPU and missing driver", 25, false, false},
		{"supported CPU and missing driver", 26, false, false},
		{"missing CPU and present driver", 26, true, true},
		{"valid identity and refused backend user", 26, true, false},
	} {
		t.Run(tt.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				root := t.TempDir()
				for rel, text := range map[string]string{
					"proc/cpuinfo": fmt.Sprintf("cpu family : %d\nmodel : 68\nmodel name : Test CPU\n", tt.family),
					"sys/devices/system/cpu/cpu0/topology/core_id": "0",
					"sys/devices/system/cpu/cpu0/topology/die_id":  "0",
				} {
					path := filepath.Join(root, rel)
					if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
						t.Fatal(err)
					}
					if err := os.WriteFile(path, []byte(text), 0644); err != nil {
						t.Fatal(err)
					}
				}
				var mb smu.Mailbox
				if tt.driverPresent {
					mb = preflightMailbox{}
					dir := filepath.Join(root, "sys/kernel/ryzen_smu_drv")
					if err := os.MkdirAll(dir, 0755); err != nil {
						t.Fatal(err)
					}
					for name, text := range map[string]string{"codename": "23", "drv_version": "0.1", "version": "57.13"} {
						if err := os.WriteFile(filepath.Join(dir, name), []byte(text), 0644); err != nil {
							t.Fatal(err)
						}
					}
				}
				if tt.missingCPU {
					if err := os.Remove(filepath.Join(root, "proc/cpuinfo")); err != nil {
						t.Fatal(err)
					}
				}
				drv, err := smu.Open(root, mb)
				if err != nil {
					t.Fatal(err)
				}
				h := host{drv: drv, pmTable: smu.NewPMTableReader(root, drv.Topology(), os.ReadFile), userErr: errors.New("backend_user not configured")}
				checks := h.Preflight()
				var names []string
				for _, check := range checks {
					names = append(names, check.Name)
				}
				wantNames := []string{"root", "cpu", "ryzen_smu", "pm_table"}
				if tt.driverPresent && !tt.missingCPU {
					wantNames = append(wantNames, "readback", "slot_mapping", "backends", "backend_user", "systemd_run")
				}
				if diff := cmp.Diff(wantNames, names); diff != "" {
					t.Fatalf("preflight checks (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff(tt.family == 26 && !tt.missingCPU, checks[1].OK); diff != "" {
					t.Fatalf("CPU validation (-want +got): %s", diff)
				}
				if diff := cmp.Diff(tt.driverPresent, checks[2].OK); diff != "" {
					t.Fatalf("driver readiness (-want +got): %s", diff)
				}
				if !tt.driverPresent && !strings.Contains(checks[2].Detail, "ryzen_smu is not loaded") {
					t.Fatalf("missing driver refusal hidden: %+v", checks[2])
				}
				if tt.family == 25 && !strings.Contains(checks[1].Detail, "not Granite Ridge") {
					t.Fatalf("unsupported CPU refusal hidden: %+v", checks[1])
				}
				if tt.missingCPU && !strings.Contains(checks[1].Detail, "cpuinfo") {
					t.Fatalf("missing CPU refusal hidden: %+v", checks[1])
				}
				if len(checks) == 9 {
					if checks[7].OK || checks[7].Detail != "backend_user not configured" {
						t.Fatalf("backend user refusal hidden: %+v", checks[7])
					}
					if checks[8].OK || !strings.HasPrefix(checks[8].Detail, "systemd-run backend_user:") {
						t.Fatalf("systemd refusal hidden: %+v", checks[8])
					}
				}
			})
		})
	}
}
