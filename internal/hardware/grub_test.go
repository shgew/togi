package hardware

import (
	"errors"
	"fmt"
	"github.com/google/go-cmp/cmp"
	"strings"
	"testing"
)

var errGRUBRunner = errors.New("injected grub-editenv failure")

type grubEnvironment struct {
	saved         string
	calls, failAt int
	after, sticky bool
}

func (e *grubEnvironment) run(_ string, args ...string) (string, error) {
	e.calls++
	fail := e.calls == e.failAt
	if fail && !e.after {
		return "", errGRUBRunner
	}
	out := ""
	switch args[0] {
	case "list":
		out = "next_entry=normal\n"
		if e.saved != "" {
			out += " saved_entry=" + e.saved + "\n"
		}
	case "unset":
		if !e.sticky {
			e.saved = ""
		}
	default:
		return "", fmt.Errorf("unexpected GRUB operation %s", args[0])
	}
	if fail {
		return "", errGRUBRunner
	}
	return out, nil
}

func TestGRUBCleanupFilesystemEffectWindows(t *testing.T) {
	for at := 1; at <= 3; at++ {
		for _, afterEffect := range []bool{false, true} {
			t.Run(fmt.Sprintf("operation %d after %v", at, afterEffect), func(t *testing.T) {
				env := &grubEnvironment{saved: "togi", failAt: at, after: afterEffect}
				grub := GRUB{Env: "test grub environment", run: env.run}
				before, after, err := grub.ClearSavedEntry()
				if !errors.Is(err, errGRUBRunner) {
					t.Fatalf("cleanup failure: %v", err)
				}
				wantBefore, wantAfter := "togi", ""
				if at == 1 {
					wantBefore = ""
				}
				if at == 2 {
					wantAfter = "togi"
				}
				if before != wantBefore || after != wantAfter {
					t.Fatalf("diagnostic before=%q after=%q, want %q %q", before, after, wantBefore, wantAfter)
				}
				unset := at == 3 || (at == 2 && afterEffect)
				if (env.saved == "") != unset {
					t.Fatalf("actual unset effect at failure: saved=%q", env.saved)
				}
				remaining := env.saved
				env.failAt = 0
				before, after, err = grub.ClearSavedEntry()
				if err != nil || before != remaining || after != "" || env.saved != "" {
					t.Fatalf("idempotent cleanup after failure: before=%q after=%q saved=%q err=%v", before, after, env.saved, err)
				}
			})
		}
	}
}

func TestGRUBCleanupRejectsRemainingSavedEntry(t *testing.T) {
	env := &grubEnvironment{saved: "togi", sticky: true}
	before, after, err := (GRUB{Env: "test grub environment", run: env.run}).ClearSavedEntry()
	if err == nil || !strings.Contains(err.Error(), "saved_entry still togi") || before != "togi" || after != "togi" || env.saved != "togi" {
		t.Fatalf("verification failure hidden: before=%q after=%q saved=%q err=%v", before, after, env.saved, err)
	}
}

func TestGRUBCleanupAlreadyUnset(t *testing.T) {
	env := &grubEnvironment{}
	before, after, err := (GRUB{Env: "test grub environment", run: env.run}).ClearSavedEntry()
	if err != nil || before != "" || after != "" || env.saved != "" {
		t.Fatalf("already-cleared environment changed: %q %q %q, %v", before, after, env.saved, err)
	}
}

func TestGRUBSavedEntryParsing(t *testing.T) {
	for _, tt := range []struct {
		name, listing, want string
	}{
		{"other keys", "next_entry=togi\nother_saved_entry=wrong\nsaved_entry_extra=wrong\n", ""},
		{"whitespace", "next_entry=normal\n \tsaved_entry=togi-specialisation \t\n", "togi-specialisation"},
		{"value contains equals", "saved_entry=menu=entry\n", "menu=entry"},
		{"empty saved entry", "saved_entry=\n", ""},
		{"no final newline", "next_entry=normal\nsaved_entry=togi", "togi"},
	} {
		t.Run(tt.name, func(t *testing.T) {
			listed := false
			grub := GRUB{Env: "test grub environment", run: func(_ string, args ...string) (string, error) {
				if args[0] != "list" || listed {
					return "", nil
				}
				listed = true
				return tt.listing, nil
			}}
			before, after, err := grub.ClearSavedEntry()
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff([]string{tt.want, ""}, []string{before, after}); diff != "" {
				t.Fatalf("saved entry (-want +got):\n%s", diff)
			}
		})
	}
}
