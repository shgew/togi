package hardware

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
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
	if diff := cmp.Diff([]machine.CoreRank{{Core: 0, Value: 125}, {Core: 8, Value: 174}}, got); diff != "" {
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

// countingMailbox fails every access, like preflightMailbox, and counts them.
type countingMailbox struct{ calls int }

func (m *countingMailbox) Command(uint32, [6]uint32) ([6]uint32, error) {
	m.calls++
	return [6]uint32{}, errors.New("mailbox unavailable")
}

func (m *countingMailbox) ReadSMN(uint32) (uint32, error) {
	m.calls++
	return 0, errors.New("mailbox unavailable")
}

// identityRoot is a sysfs root whose CPU and ryzen_smu identity checks pass.
func identityRoot(t *testing.T) string {
	t.Helper()
	root := t.TempDir()
	for rel, text := range map[string]string{
		"proc/cpuinfo": "cpu family : 26\nmodel : 68\nmodel name : Test CPU\n",
		"sys/devices/system/cpu/cpu0/topology/core_id": "0",
		"sys/devices/system/cpu/cpu0/topology/die_id":  "0",
		"sys/kernel/ryzen_smu_drv/codename":            "23",
		"sys/kernel/ryzen_smu_drv/drv_version":         "0.1",
		"sys/kernel/ryzen_smu_drv/version":             "57.13",
	} {
		path := filepath.Join(root, rel)
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(text), 0644); err != nil {
			t.Fatal(err)
		}
	}
	return root
}

func checkNames(checks []machine.Check) []string {
	names := make([]string, len(checks))
	for i, c := range checks {
		names[i] = c.Name
	}
	return names
}

func TestDiagnoseUnprivilegedMakesNoMailboxCalls(t *testing.T) {
	mb := &countingMailbox{}
	ran, skipped, err := diagnose(identityRoot(t), mb, config.Config{}, &machine.BIOSContext{BIOSVersion: "3.14"}, false)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(0, mb.calls); diff != "" {
		t.Fatalf("mailbox calls (-want +got): %s", diff)
	}
	if diff := cmp.Diff([]string{"cpu", "ryzen_smu", "backends", "backend_user", "watchdog"}, checkNames(ran)); diff != "" {
		t.Fatalf("checks run (-want +got):\n%s", diff)
	}
	if !ran[0].OK || !ran[1].OK {
		t.Fatalf("identity checks failed: %+v", ran[:2])
	}
	var want []machine.Check
	for _, name := range []string{"pm_table", "readback", "slot_mapping", "systemd_run", "bios_context"} {
		want = append(want, machine.Check{Name: name, Detail: "needs root"})
	}
	if diff := cmp.Diff(want, skipped); diff != "" {
		t.Fatalf("skipped checks (-want +got):\n%s", diff)
	}
}

func TestDiagnoseCheckSetsPartitionPreflight(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		root := identityRoot(t)
		drv, err := smu.Open(root, preflightMailbox{})
		if err != nil {
			t.Fatal(err)
		}
		h := host{drv: drv, pmTable: smu.NewPMTableReader(root, drv.Topology(), os.ReadFile), userErr: errors.New("backend_user not configured")}
		preflight := checkNames(h.Preflight())
		if diff := cmp.Diff("root", preflight[0]); diff != "" {
			t.Fatalf("first preflight check (-want +got): %s", diff)
		}
		unprivileged, needsRoot, err := diagnose(root, &countingMailbox{}, config.Config{}, nil, false)
		if err != nil {
			t.Fatal(err)
		}
		privileged, skipped, err := diagnose(root, preflightMailbox{}, config.Config{}, nil, true)
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff([]machine.Check(nil), skipped); diff != "" {
			t.Fatalf("privileged skipped checks (-want +got):\n%s", diff)
		}
		if diff := cmp.Diff(append(slices.Clone(preflight[1:]), "watchdog"), checkNames(privileged)); diff != "" {
			t.Fatalf("privileged checks (-want +got):\n%s", diff)
		}
		union := slices.Concat(checkNames(unprivileged[:len(unprivileged)-1]), checkNames(needsRoot))
		slices.Sort(union)
		want := slices.Clone(preflight[1:])
		slices.Sort(want)
		if diff := cmp.Diff(want, union); diff != "" {
			t.Fatalf("unprivileged and root-only checks do not partition preflight (-want +got):\n%s", diff)
		}
	})
}

func TestDiagnoseComparesBIOSContextOnlyAfterPassingChecks(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		mb := &countingMailbox{}
		ran, skipped, err := diagnose(identityRoot(t), mb, config.Config{}, &machine.BIOSContext{BIOSVersion: "3.14"}, true)
		if err != nil {
			t.Fatal(err)
		}
		if slices.Contains(checkNames(ran), "bios_context") {
			t.Fatalf("bios_context compared after failed checks: %+v", ran)
		}
		if diff := cmp.Diff([]machine.Check{{Name: "bios_context", Detail: "needs every other check to pass"}}, skipped); diff != "" {
			t.Fatalf("skipped checks (-want +got):\n%s", diff)
		}
	})
}

// boostMailbox answers the boost limit command 0x6e and fails every other access.
type boostMailbox struct{}

func (boostMailbox) Command(cmd uint32, _ [6]uint32) ([6]uint32, error) {
	if cmd == 0x6e {
		return [6]uint32{5750}, nil
	}
	return [6]uint32{}, errors.New("mailbox unavailable")
}

func (boostMailbox) ReadSMN(uint32) (uint32, error) {
	return 0, errors.New("mailbox unavailable")
}

func TestBIOSContextCheck(t *testing.T) {
	recorded := machine.BIOSContext{BIOSVersion: "3.14", Board: "ASRock X870E Taichi", CPUModel: "Test CPU", Microcode: "0xb404038", BoostLimitMHz: 5750}
	changed := recorded
	changed.BIOSVersion = "3.10"
	for _, tc := range []struct {
		name     string
		recorded machine.BIOSContext
		missing  string
		want     machine.Check
	}{
		{name: "match", recorded: recorded, want: machine.Check{Name: "bios_context", Detail: "matches the session", OK: true}},
		{name: "mismatch", recorded: changed, want: machine.Check{Name: "bios_context", Detail: "bios_version is 3.14; the session recorded 3.10; run archives this session and starts a new one"}},
		{name: "read error", recorded: recorded, missing: "sys/class/dmi/id/board_name"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				root := identityRoot(t)
				for rel, text := range map[string]string{
					"proc/cpuinfo":                  "cpu family : 26\nmodel : 68\nmodel name : Test CPU\nmicrocode : 0xb404038\n",
					"sys/class/dmi/id/bios_version": "3.14\n",
					"sys/class/dmi/id/board_vendor": "ASRock\n",
					"sys/class/dmi/id/board_name":   "X870E Taichi\n",
				} {
					path := filepath.Join(root, rel)
					if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
						t.Fatal(err)
					}
					if err := os.WriteFile(path, []byte(text), 0644); err != nil {
						t.Fatal(err)
					}
				}
				if tc.missing != "" {
					if err := os.Remove(filepath.Join(root, tc.missing)); err != nil {
						t.Fatal(err)
					}
				}
				drv, err := smu.Open(root, boostMailbox{})
				if err != nil {
					t.Fatal(err)
				}
				got, err := biosContextCheck(drv, tc.recorded)
				if tc.missing != "" {
					if !errors.Is(err, fs.ErrNotExist) || !strings.HasPrefix(err.Error(), "read BIOS context: ") {
						t.Fatalf("read error = %v, want a read BIOS context error", err)
					}
					return
				}
				if err != nil {
					t.Fatal(err)
				}
				if diff := cmp.Diff(tc.want, got); diff != "" {
					t.Fatalf("bios_context check (-want +got):\n%s", diff)
				}
			})
		})
	}
}
