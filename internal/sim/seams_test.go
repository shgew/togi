package sim

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestFaultsAndRanking(t *testing.T) {
	if ranking, err := newMachine(t, Config{Cores: 2}).Seams().Host.Ranking(); err == nil || ranking != nil {
		t.Fatalf("unavailable ranking: %v %v", ranking, err)
	}
	m := newMachine(t, Config{Cores: 2, Ranking: []int{11, 22}, OldKernel: true})
	ranking, err := m.Seams().Host.Ranking()
	if err != nil || !cmp.Equal(ranking, []machine.CoreRank{{Core: 0, Value: 11}, {Core: 1, Value: 22}}) {
		t.Fatalf("ranking %v %v", ranking, err)
	}
	ranking[0].Value = 99
	if got, _ := m.Seams().Host.Ranking(); got[0].Value != 11 {
		t.Fatalf("ranking aliased: %v", got)
	}
	m.FailWriteAt(2)
	if err := m.Seams().SMU.SetOffset(0, -1); err != nil {
		t.Fatal(err)
	}
	if err := m.Seams().SMU.SetOffset(1, -2); err == nil {
		t.Fatal("second write succeeded")
	}
	if o, _ := m.Seams().SMU.Offset(1); o != 0 {
		t.Fatalf("failed write applied %d", o)
	}
	m.CrashAtWrite(1)
	if err := m.Seams().SMU.SetOffset(1, -2); !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("crash at write: %v", err)
	}
	m.Reboot()
	m.MissBackend(machine.Mprime)
	_, err = m.Seams().Trials.Start(context.Background(), machine.TrialSpec{Workload: machine.Workload{Backend: machine.Mprime}})
	if !errors.Is(err, machine.ErrBackendMissing) {
		t.Fatalf("missing backend: %v", err)
	}
	m.PowerLoss()
	m.Reboot()
	boot, _ := m.Seams().Host.BootID()
	reason, _ := m.Seams().Kernel.ResetReason(boot)
	if reason.Kind != "" || reason.Supported || reason.Raw != "" {
		t.Fatalf("old kernel power loss: %+v", reason)
	}
	m.HoldPowerButton()
	m.Reboot()
	boot, _ = m.Seams().Host.BootID()
	reason, _ = m.Seams().Kernel.ResetReason(boot)
	if reason.Kind != "" || reason.Supported || reason.Raw != "" {
		t.Fatalf("old kernel power button: %+v", reason)
	}
}

func TestHostValidationAndWatchdogFaults(t *testing.T) {
	m := newMachine(t, Config{Cores: 2})
	for _, name := range []string{"cpu", "ryzen_smu"} {
		m.FailCheck(name, "unsupported")
		if err := m.Seams().Host.ValidateSMU(); err == nil || err.Error() != "validate "+name+": unsupported" {
			t.Fatalf("validate = %v", err)
		}
		m.FailCheck(name, "")
	}
	m.FailCheck("root", "not root")
	if err := m.Seams().Host.ValidateSMU(); err != nil {
		t.Fatalf("unrelated preflight blocked SMU validation: %v", err)
	}
	for _, detail := range []string{"not armed", ""} {
		m.FailCheck("watchdog", detail)
		check := m.Seams().Host.Watchdog()
		if check.Name != "watchdog" || check.OK != (detail == "") || (detail != "" && check.Detail != detail) {
			t.Fatalf("watchdog = %+v", check)
		}
	}
}

func TestInvalidSMUCoreDoesNotWrite(t *testing.T) {
	m := newMachine(t, Config{Cores: 2, BIOS: []int{-3, -4}})
	for _, core := range []int{-1, 2} {
		if _, err := m.Seams().SMU.Offset(core); err == nil || !strings.Contains(err.Error(), "no core") {
			t.Fatalf("read core %d: %v", core, err)
		}
		if err := m.Seams().SMU.SetOffset(core, -10); err == nil || !strings.Contains(err.Error(), "no core") {
			t.Fatalf("write core %d: %v", core, err)
		}
	}
	if diff := cmp.Diff([]int{-3, -4}, m.regs); diff != "" {
		t.Fatal(diff)
	}
}

func TestCanceledStartDoesNotConsumeFault(t *testing.T) {
	m := newMachine(t, Config{Cores: 2})
	m.FailSetup(1)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	spec := machine.TrialSpec{ID: "0001", Regime: machine.R1, Workload: machine.PickWorkload(machine.R1, 0), Condition: machine.Alone, Cores: []int{0}, CPUs: []int{0}, Duration: time.Minute}
	if _, err := m.Seams().Trials.Start(ctx, spec); !errors.Is(err, context.Canceled) {
		t.Fatalf("start = %v", err)
	}
	if _, err := m.Seams().Trials.Start(context.Background(), spec); err == nil || !strings.Contains(err.Error(), "setup failure") {
		t.Fatalf("fault consumed by canceled start: %v", err)
	}
	if !m.Now().Equal(epoch) {
		t.Fatal("canceled start advanced clock")
	}
}

func TestStopMatchesCanceledWaitCleanup(t *testing.T) {
	for _, state := range []string{"running", "crashed", "rebooted"} {
		t.Run(state, func(t *testing.T) {
			m := newMachine(t, Config{Cores: 2})
			spec := machine.TrialSpec{ID: "0001", Regime: machine.R1, Workload: machine.PickWorkload(machine.R1, 0), Condition: machine.Alone, Cores: []int{0}, CPUs: []int{0}, Duration: time.Minute}
			run, err := m.Seams().Trials.Start(context.Background(), spec)
			if err != nil {
				t.Fatal(err)
			}
			switch state {
			case "crashed":
				m.Crash()
			case "rebooted":
				m.Reboot()
			}
			before := m.Now()
			ctx, cancel := context.WithCancel(context.Background())
			cancel()
			_, want := run.Wait(ctx, nil)
			if errors.Is(want, context.Canceled) {
				want = nil
			}
			got := run.Stop()
			if !errors.Is(got, want) {
				t.Fatalf("stop = %v, canceled wait cleanup = %v", got, want)
			}
			if diff := cmp.Diff(before, m.Now()); diff != "" {
				t.Fatal(diff)
			}
			if state == "running" {
				m.Crash()
			} else {
				m.Reboot()
			}
			if again := run.Stop(); !errors.Is(again, got) {
				t.Fatalf("repeated stop = %v, first result = %v", again, got)
			}
		})
	}
}

func TestCrashedSeamsRejectOperations(t *testing.T) {
	m := newMachine(t, Config{Cores: 2})
	s := m.Seams()
	boot, err := s.Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	m.Crash()
	start := m.Now()
	spec := machine.TrialSpec{ID: "0001", Regime: machine.R1, Workload: machine.PickWorkload(machine.R1, 0), Condition: machine.Alone, Cores: []int{0}, CPUs: []int{0}, Duration: time.Minute}
	for name, operation := range map[string]func() error{
		"sleep":         func() error { return m.Sleep(context.Background(), time.Hour) },
		"offset":        func() error { _, err := s.SMU.Offset(0); return err },
		"write":         func() error { return s.SMU.SetOffset(0, -10) },
		"topology":      func() error { _, err := s.Host.Topology(); return err },
		"ranking":       func() error { _, err := s.Host.Ranking(); return err },
		"BIOS":          func() error { _, err := s.Host.BIOSContext(); return err },
		"MCEs":          func() error { _, err := s.Kernel.MCEs(boot, 0); return err },
		"kernel cursor": func() error { _, err := s.Kernel.ReadMCEs(boot, ""); return err },
		"reset":         func() error { _, err := s.Kernel.ResetReason(boot); return err },
		"reset after":   func() error { _, err := s.Kernel.ResetReasonAfter(boot); return err },
		"pstore":        func() error { _, err := s.Kernel.SavedPstore(boot); return err },
		"start":         func() error { _, err := s.Trials.Start(context.Background(), spec); return err },
	} {
		t.Run(name, func(t *testing.T) {
			if err := operation(); !errors.Is(err, machine.ErrCrashed) {
				t.Fatalf("error = %v", err)
			}
		})
	}
	if !m.Now().Equal(start) {
		t.Fatal("crashed operations advanced clock")
	}
	m.Reboot()
	if got, err := s.SMU.Offset(0); err != nil || got != 0 {
		t.Fatalf("crashed write changed boot offset: %d, %v", got, err)
	}
}

func TestTopologyAndConfiguredClock(t *testing.T) {
	start := epoch.Add(7 * time.Hour)
	for _, count := range []int{0, 2, 4} {
		m := newMachine(t, Config{Cores: count, Start: start})
		n := count
		if n == 0 {
			n = 16
		}
		want := make([]machine.CoreInfo, n)
		for c := range want {
			want[c] = machine.CoreInfo{Core: c, CCD: c / (n / 2), CPUs: []int{c, c + n}}
		}
		got, err := m.Seams().Host.Topology()
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(want, got); diff != "" {
			t.Fatal(diff)
		}
		got[0].CPUs[0] = 99
		again, err := m.Seams().Host.Topology()
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(want, again); diff != "" {
			t.Fatal(diff)
		}
		if !m.Now().Equal(start) || m.Monotonic() != 0 {
			t.Fatalf("start clock = %s / %s", m.Now(), m.Monotonic())
		}
	}
}

func TestKernelCursorAcrossEmptyBoundariesAndReboot(t *testing.T) {
	m := newMachine(t, Config{Cores: 2, Limits: flat(2, -50, -50), Script: map[string]Outcome{"0001": {Signal: machine.CorrectedMCE, AtS: 7, Core: 0}}})
	k := m.Seams().Kernel
	boot, _ := m.Seams().Host.BootID()
	first, err := k.ReadMCEs(boot, "")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), []int{0}, 10*time.Second, nil); err != nil {
		t.Fatal(err)
	}
	read, err := k.ReadMCEs(boot, first.Cursor)
	if err != nil || len(read.MCEs) != 1 || read.MCEs[0].Monotonic != 7*time.Second {
		t.Fatalf("MCE after empty boundary: %+v %v", read, err)
	}
	empty, err := k.ReadMCEs(boot, read.Cursor)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(machine.KernelRead{Cursor: read.Cursor}, empty); diff != "" {
		t.Fatal(diff)
	}
	m.Reboot()
	nextBoot, _ := m.Seams().Host.BootID()
	if _, err := k.ReadMCEs(nextBoot, read.Cursor); !errors.Is(err, machine.ErrCursorMissing) {
		t.Fatalf("accepted previous boot's cursor: %v", err)
	}
	next, err := k.ReadMCEs(nextBoot, "")
	if err != nil || next.Cursor == read.Cursor || len(next.MCEs) != 0 {
		t.Fatalf("new boot boundary: %+v %v", next, err)
	}
	tail, err := k.ReadMCEs(boot, first.Cursor)
	if err != nil || len(tail.MCEs) != 1 {
		t.Fatalf("persistent previous boot tail: %+v %v", tail, err)
	}
}

func TestKernelMissingBootsAndCursors(t *testing.T) {
	m := newMachine(t, Config{Cores: 2})
	k := m.Seams().Kernel
	boot, err := m.Seams().Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := k.ReadMCEs("missing", ""); !errors.Is(err, machine.ErrBootMissing) {
		t.Fatalf("read = %v", err)
	}
	if _, err := k.ResetReason("missing"); !errors.Is(err, machine.ErrBootMissing) {
		t.Fatalf("reason = %v", err)
	}
	for _, old := range []string{"missing", boot} {
		if _, err := k.ResetReasonAfter(old); !errors.Is(err, machine.ErrBootMissing) {
			t.Fatalf("successor = %v", err)
		}
	}
	for _, suffix := range []string{"no-number", "-1", "1"} {
		if _, err := k.ReadMCEs(boot, boot+":"+suffix); !errors.Is(err, machine.ErrCursorMissing) {
			t.Fatalf("cursor %s = %v", suffix, err)
		}
	}
	m.NextReset(machine.ResetThermalTrip)
	m.Crash()
	m.Reboot()
	m.PowerLoss()
	m.Reboot()
	got, err := k.ResetReasonAfter(boot)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(resetReason(machine.ResetThermalTrip, true), got); diff != "" {
		t.Fatal(diff)
	}
}

func TestOldKernelDoesNotReportResetKind(t *testing.T) {
	t.Parallel()
	m := newMachine(t, Config{Cores: 2, OldKernel: true})
	m.NextReset(machine.ResetThermalTrip)
	m.Crash()
	m.Reboot()
	boot, err := m.Seams().Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	reason, err := m.Seams().Kernel.ResetReason(boot)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(machine.ResetReason{}, reason); diff != "" {
		t.Fatalf("unsupported reset reason (-want +got): %s", diff)
	}
}

func TestResetReasonKinds(t *testing.T) {
	for _, tc := range []struct {
		kind machine.ResetKind
		line string
	}{
		{machine.ResetWatchdog, "hardware watchdog timer expired"},
		{machine.ResetSyncFlood, "an uncorrected error caused a data fabric sync flood event"},
		{machine.ResetCPUShutdown, "internal CPU shutdown event occurred"},
		{machine.ResetPowerButton, "power button was pressed for 4 seconds"},
		{machine.ResetThermalTrip, "thermal pin BP_THERMTRIP_L was tripped"},
		{machine.ResetUnknown, "unrecognized reset reason"},
		{machine.ResetPowerLoss, ""},
	} {
		t.Run(string(tc.kind), func(t *testing.T) {
			m := newMachine(t, Config{Cores: 2})
			m.NextReset(tc.kind)
			m.Crash()
			m.Reboot()
			boot, _ := m.Seams().Host.BootID()
			reason, err := m.Seams().Kernel.ResetReason(boot)
			wantKind := tc.kind
			if tc.kind == machine.ResetPowerLoss {
				wantKind = ""
			}
			if err != nil || reason.Kind != wantKind || !reason.Supported {
				t.Fatalf("reason %+v %v", reason, err)
			}
			if tc.line != "" && !strings.Contains(reason.Raw, tc.line) || tc.line == "" && reason.Raw != "" {
				t.Fatalf("kernel line %q, want %q", reason.Raw, tc.line)
			}
		})
	}
}
