package detect

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func kernelLines(lines ...string) []byte {
	var out []byte
	for _, line := range lines {
		encoded, _ := json.Marshal(struct {
			Message string `json:"MESSAGE"`
		}{line})
		out = append(out, encoded...)
		out = append(out, '\n')
	}
	return out
}

func TestResetReason(t *testing.T) {
	for _, tt := range []struct {
		name, version string
		texts         []string
		kind          machine.ResetKind
		supported     bool
	}{
		{"old kernel", "6.15", nil, "", false},
		{"boundary", "6.16", nil, "", true},
		{"new major", "7.0", nil, "", true},
		{"watchdog", "6.16", []string{"hardware watchdog timer expired"}, machine.ResetWatchdog, true},
		{"fabric flood", "6.16", []string{"an uncorrected error caused a data fabric sync flood event"}, machine.ResetSyncFlood, true},
		{"software flood", "6.16", []string{"a software sync flood event occurred"}, machine.ResetSyncFlood, true},
		{"cpu shutdown", "6.16", []string{"internal CPU shutdown event occurred"}, machine.ResetCPUShutdown, true},
		{"power button", "6.16", []string{"power button was pressed for 4 seconds"}, machine.ResetPowerButton, true},
		{"thermal pin", "6.16", []string{"thermal pin BP_THERMTRIP_L was tripped"}, machine.ResetThermalTrip, true},
		{"thermal limit", "6.16", []string{"internal CPU thermal limit was tripped"}, machine.ResetThermalTrip, true},
		{"unknown", "6.16", []string{"unexpected reason"}, machine.ResetUnknown, true},
		{"precedence", "6.16", []string{"power button was pressed for 4 seconds", "hardware watchdog timer expired", "internal CPU thermal limit was tripped", "an uncorrected error caused a data fabric sync flood event"}, machine.ResetThermalTrip, true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			lines := []string{"Linux version " + tt.version + ".0-test"}
			for _, text := range tt.texts {
				lines = append(lines, "x86/amd: Previous system reset reason [0x00000001]: "+text)
			}
			k := NewKernel(nil)
			k.journalctl = func(args []string) ([]byte, []byte, int, error) {
				want := []string{"-k", "-b", "1234", "-o", "json", "--output-fields=MESSAGE", "--grep", "Linux version|Previous system reset reason", "--no-pager", "-q"}
				if diff := cmp.Diff(want, args); diff != "" {
					t.Fatalf("args (-want +got):\n%s", diff)
				}
				return kernelLines(lines...), nil, 0, nil
			}
			got, err := k.ResetReason("12-34")
			if err != nil {
				t.Fatal(err)
			}
			want := machine.ResetReason{Kind: tt.kind, Raw: strings.Join(tt.texts, "\n"), Supported: tt.supported}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("reason (-want +got):\n%s", diff)
			}
		})
	}
}

func TestResetReasonMissingBoot(t *testing.T) {
	k := NewKernel(nil)
	k.journalctl = func([]string) ([]byte, []byte, int, error) { return nil, []byte("No journal boot entry found"), 1, nil }
	if _, err := k.ResetReason("missing"); !errors.Is(err, machine.ErrBootMissing) {
		t.Fatalf("ResetReason of a vacuumed boot: %v, want ErrBootMissing", err)
	}
}

func TestResetReasonWithoutMatchingLines(t *testing.T) {
	k := NewKernel(nil)
	k.journalctl = func([]string) ([]byte, []byte, int, error) { return nil, nil, 1, nil }
	got, err := k.ResetReason("partial")
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(machine.ResetReason{}, got); diff != "" {
		t.Fatalf("reason (-want +got):\n%s", diff)
	}
}

func TestResetReasonAfter(t *testing.T) {
	for _, tc := range []struct {
		name    string
		boots   string
		missing bool
		want    machine.ResetReason
	}{
		{"immediate non-togi boot", `[{"boot_id":"aabb"},{"boot_id":"desktop"},{"boot_id":"current"}]`, false, machine.ResetReason{Kind: machine.ResetWatchdog, Raw: "hardware watchdog timer expired", Supported: true}},
		{"crashed boot missing", `[{"boot_id":"desktop"},{"boot_id":"current"}]`, true, machine.ResetReason{}},
		{"no successor", `[{"boot_id":"aabb"}]`, true, machine.ResetReason{}},
		{"no retained boots", `[]`, true, machine.ResetReason{}},
		{"successor log vacuumed", `[{"boot_id":"aabb"},{"boot_id":"vacuumed"},{"boot_id":"current"}]`, true, machine.ResetReason{}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			k := NewKernel(nil)
			k.journalctl = func(args []string) ([]byte, []byte, int, error) {
				if args[0] == "--list-boots" {
					return []byte(tc.boots), nil, 0, nil
				}
				if args[0] == "-b" {
					if args[1] == "aabb" {
						return bootBoundary("aabb", "series", "100"), nil, 0, nil
					}
					return bootBoundary(args[1], "series", "101"), nil, 0, nil
				}
				switch args[2] {
				case "desktop":
					return kernelLines("Linux version 7.2.8", "x86/amd: Previous system reset reason [0x00000001]: hardware watchdog timer expired"), nil, 0, nil
				case "vacuumed":
					return nil, []byte("No journal boot entry found"), 1, nil
				default:
					return kernelLines("Linux version 7.2.8", "x86/amd: Previous system reset reason [0x00000001]: internal CPU thermal limit was tripped"), nil, 0, nil
				}
			}
			got, err := k.ResetReasonAfter("aa-bb")
			if tc.missing {
				if !errors.Is(err, machine.ErrBootMissing) {
					t.Fatalf("missing successor: %v, want ErrBootMissing", err)
				}
			} else if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatalf("reason (-want +got):\n%s", diff)
			}
		})
	}
}

func bootBoundary(boot, sequenceID, sequence string) []byte {
	data, _ := json.Marshal(map[string]string{"_BOOT_ID": boot, "__SEQNUM_ID": sequenceID, "__SEQNUM": sequence})
	return data
}

func TestResetReasonAfterRequiresContinuousJournal(t *testing.T) {
	for _, tc := range []struct {
		name        string
		last, first []byte
		missing     bool
	}{
		{"continuous boundary", bootBoundary("old", "series", "100"), bootBoundary("later", "series", "101"), false},
		{"omitted intermediate boot", bootBoundary("old", "series", "100"), bootBoundary("later", "series", "200"), true},
		{"changed sequence identity", bootBoundary("old", "series", "100"), bootBoundary("later", "other", "101"), true},
		{"missing predecessor metadata", []byte(`{"_BOOT_ID":"old"}`), bootBoundary("later", "series", "101"), true},
		{"missing successor metadata", bootBoundary("old", "series", "100"), []byte(`{"_BOOT_ID":"later"}`), true},
		{"missing predecessor tail", nil, bootBoundary("later", "series", "101"), true},
		{"missing successor start", bootBoundary("old", "series", "100"), nil, true},
		{"wrong boundary boot", bootBoundary("other", "series", "100"), bootBoundary("later", "series", "101"), true},
		{"sequence wraparound", bootBoundary("old", "series", "18446744073709551615"), bootBoundary("later", "series", "0"), true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			k := NewKernel(nil)
			k.journalctl = func(args []string) ([]byte, []byte, int, error) {
				switch args[0] {
				case "--list-boots":
					return []byte(`[{"boot_id":"old"},{"boot_id":"later"}]`), nil, 0, nil
				case "-b":
					if args[1] == "old" {
						return tc.last, nil, 0, nil
					}
					return tc.first, nil, 0, nil
				default:
					return kernelLines("Linux version 7.2.8", "x86/amd: Previous system reset reason [0x00000001]: internal CPU thermal limit was tripped"), nil, 0, nil
				}
			}
			got, err := k.ResetReasonAfter("old")
			if tc.missing {
				if !errors.Is(err, machine.ErrBootMissing) {
					t.Fatalf("unproven successor: %+v, %v, want ErrBootMissing", got, err)
				}
				if diff := cmp.Diff(machine.ResetReason{}, got); diff != "" {
					t.Fatalf("unproven successor supplied a reason (-want +got):\n%s", diff)
				}
			} else {
				if err != nil {
					t.Fatal(err)
				}
				want := machine.ResetReason{Kind: machine.ResetThermalTrip, Raw: "internal CPU thermal limit was tripped", Supported: true}
				if diff := cmp.Diff(want, got); diff != "" {
					t.Fatalf("continuous successor reason (-want +got):\n%s", diff)
				}
			}
		})
	}
}

func TestResetReasonAfterBoundaryErrors(t *testing.T) {
	for _, tc := range []struct {
		name   string
		out    string
		stderr string
		code   int
		err    error
		want   error
	}{
		{"vacuumed boundary", "", "No journal boot entry found", 1, nil, machine.ErrBootMissing},
		{"empty boundary", "", "", 1, nil, machine.ErrBootMissing},
		{"command error", "", "", 0, context.Canceled, context.Canceled},
		{"permission error", "", "permission denied", 1, nil, nil},
		{"malformed boundary", "{", "", 0, nil, nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			k := NewKernel(nil)
			k.journalctl = func(args []string) ([]byte, []byte, int, error) {
				if args[0] == "--list-boots" {
					return []byte(`[{"boot_id":"old"},{"boot_id":"later"}]`), nil, 0, nil
				}
				return []byte(tc.out), []byte(tc.stderr), tc.code, tc.err
			}
			got, err := k.ResetReasonAfter("old")
			if tc.want != nil {
				if !errors.Is(err, tc.want) {
					t.Fatalf("boundary error: %v, want %v", err, tc.want)
				}
			} else if err == nil || errors.Is(err, machine.ErrBootMissing) {
				t.Fatalf("unreadable boundary: %v", err)
			}
			if diff := cmp.Diff(machine.ResetReason{}, got); diff != "" {
				t.Fatalf("boundary error supplied a reason (-want +got):\n%s", diff)
			}
		})
	}
}

func TestResetReasonAfterWithoutSystemJournal(t *testing.T) {
	k := NewKernel(nil)
	k.journalctl = func([]string) ([]byte, []byte, int, error) { return nil, nil, 1, nil }
	got, err := k.ResetReasonAfter("crashed")
	if !errors.Is(err, machine.ErrBootMissing) {
		t.Fatalf("empty system journal: %v, want ErrBootMissing", err)
	}
	if diff := cmp.Diff(machine.ResetReason{}, got); diff != "" {
		t.Fatalf("reason (-want +got):\n%s", diff)
	}
}

func TestResetReasonAfterBootListErrors(t *testing.T) {
	for _, tc := range []struct {
		name   string
		output string
		stderr string
		code   int
		err    error
	}{
		{"command error", "", "", 0, errors.New("journal unavailable")},
		{"nonzero exit", "", "permission denied", 1, nil},
		{"malformed list", "[", "", 0, nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			k := NewKernel(nil)
			k.journalctl = func([]string) ([]byte, []byte, int, error) {
				return []byte(tc.output), []byte(tc.stderr), tc.code, tc.err
			}
			_, err := k.ResetReasonAfter("crashed")
			if err == nil || errors.Is(err, machine.ErrBootMissing) {
				t.Fatalf("unreadable boot list: %v", err)
			}
			if tc.err != nil && !errors.Is(err, tc.err) {
				t.Fatalf("command error lost: %v", err)
			}
		})
	}
}

func TestMCEsMonotonic(t *testing.T) {
	k := NewKernel([]machine.CoreInfo{{Core: 3, CPUs: []int{2}}})
	const status = "[Hardware Error]: CPU:2 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135"
	var lines []byte
	for _, micros := range []string{"1000000", "2000000"} {
		encoded, err := json.Marshal(map[string]string{"MESSAGE": status, "__REALTIME_TIMESTAMP": "3000000", "__MONOTONIC_TIMESTAMP": micros})
		if err != nil {
			t.Fatal(err)
		}
		lines = append(lines, encoded...)
		lines = append(lines, '\n')
	}
	k.journalctl = func([]string) ([]byte, []byte, int, error) { return lines, nil, 0, nil }
	got, err := k.MCEs("boot", 1500*time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 {
		t.Fatalf("MCEs = %+v", got)
	}
	if diff := cmp.Diff(2*time.Second, got[0].Monotonic); diff != "" {
		t.Fatalf("monotonic (-want +got):\n%s", diff)
	}
}

func TestResetReasonMalformedOutputDoesNotInventReason(t *testing.T) {
	for _, tc := range []struct {
		name string
		out  []byte
		code int
	}{
		{"command exit", nil, 2},
		{"invalid JSON", []byte("{"), 0},
		{"invalid message", []byte(`{"MESSAGE":{}}`), 0},
	} {
		t.Run(tc.name, func(t *testing.T) {
			k := NewKernel(nil)
			k.journalctl = func([]string) ([]byte, []byte, int, error) { return tc.out, nil, tc.code, nil }
			got, err := k.ResetReason("boot")
			if err == nil {
				t.Fatal("unreadable reason accepted")
			}
			if diff := cmp.Diff(machine.ResetReason{}, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
	raw := []byte(`{"MESSAGE":[76,105,110,117,120,32,118,101,114,115,105,111,110,32,55,46,48]}`)
	k := NewKernel(nil)
	k.journalctl = func([]string) ([]byte, []byte, int, error) { return raw, nil, 0, nil }
	got, err := k.ResetReason("boot")
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(machine.ResetReason{Supported: true}, got); diff != "" {
		t.Fatal(diff)
	}
}
