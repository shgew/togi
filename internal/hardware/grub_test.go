package hardware

import (
	"errors"
	"fmt"
	"github.com/google/go-cmp/cmp"
	"strings"
	"testing"

	"github.com/shgew/togi/internal/tuningboot"
)

var errGRUBRunner = errors.New("injected grub-editenv failure")

type grubEnvironment struct {
	saved         string
	calls, failAt int
	after, sticky bool
	values        map[string]string
	operations    [][]string
}

func (e *grubEnvironment) run(_ string, args ...string) (string, error) {
	e.calls++
	e.operations = append(e.operations, append([]string(nil), args...))
	fail := e.calls == e.failAt
	if fail && !e.after {
		return "", errGRUBRunner
	}
	out := ""
	switch args[0] {
	case "list":
		out = "next_entry=normal\n"
		if e.saved != "" {
			out += "saved_entry=" + e.saved + "\n"
		}
		for name, value := range e.values {
			out += name + "=" + value + "\n"
		}
	case "unset":
		for _, name := range args[1:] {
			if name == "saved_entry" {
				if !e.sticky {
					e.saved = ""
				}
			} else {
				delete(e.values, name)
			}
		}
	case "set":
		if e.values == nil {
			e.values = make(map[string]string)
		}
		for _, assignment := range args[1:] {
			name, value, ok := strings.Cut(assignment, "=")
			if !ok {
				return "", fmt.Errorf("invalid GRUB assignment %q", assignment)
			}
			e.values[name] = value
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
		{"value whitespace", "next_entry=normal\nsaved_entry= togi-specialisation \t\n", " togi-specialisation \t"},
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

func TestGRUBEnvironmentOperations(t *testing.T) {
	env := &grubEnvironment{saved: "togi"}
	grub := GRUB{Env: "test grub environment", run: env.run}
	values := map[string]string{"togi_restart_count": "2", "togi_leave_reason": `{"id":"reason","count":2,"reason":"retry"}`}
	if err := grub.Set(values); err != nil {
		t.Fatal(err)
	}
	wantSet := []string{"set", "togi_leave_reason=" + values["togi_leave_reason"], "togi_restart_count=2"}
	if diff := cmp.Diff(wantSet, env.operations[0]); diff != "" {
		t.Fatalf("one deterministic set (-want +got): %s", diff)
	}
	for name, want := range values {
		got, err := grub.Get(name)
		if err != nil || got != want {
			t.Fatalf("Get(%s) = %q, %v; want %q", name, got, err, want)
		}
	}
	if got, err := grub.Get("missing"); err != nil || got != "" {
		t.Fatalf("absent variable = %q, %v", got, err)
	}
	if err := grub.Unset("togi_restart_count", "togi_leave_reason"); err != nil {
		t.Fatal(err)
	}
	if len(env.values) != 0 || env.saved != "togi" {
		t.Fatalf("unset changed unrelated entry: %+v", env)
	}
}

func TestGRUBRejectsInvalidEnvironmentValues(t *testing.T) {
	for _, tt := range []struct {
		name, variable, value string
	}{
		{"newline value", "reason", "retry\nsaved_entry=togi"},
		{"nul value", "reason", "retry\x00"},
		{"non ASCII", "reason", "retry ☃"},
		{"oversized value", "reason", strings.Repeat("a", 301)},
		{"empty name", "", "retry"},
		{"assignment name", "reason=saved_entry", "retry"},
		{"newline name", "reason\n", "retry"},
		{"oversized name", strings.Repeat("a", 65), "retry"},
	} {
		t.Run(tt.name, func(t *testing.T) {
			env := &grubEnvironment{}
			grub := GRUB{run: env.run}
			if err := grub.Set(map[string]string{tt.variable: tt.value}); err == nil || env.calls != 0 {
				t.Fatalf("invalid set reached GRUB: calls=%d err=%v", env.calls, err)
			}
			if tt.variable == "reason" {
				return
			}
			if _, err := grub.Get(tt.variable); err == nil || env.calls != 0 {
				t.Fatalf("invalid get reached GRUB: calls=%d err=%v", env.calls, err)
			}
			if err := grub.Unset("saved_entry", tt.variable); err == nil || env.calls != 0 {
				t.Fatalf("invalid unset reached GRUB: calls=%d err=%v", env.calls, err)
			}
		})
	}
}

func TestGRUBRetryRejectsWhitespaceCounts(t *testing.T) {
	t.Parallel()
	for _, value := range []string{" ", " 1", "1 ", "\t2", "3\r"} {
		t.Run(fmt.Sprintf("%q", value), func(t *testing.T) {
			t.Parallel()
			env := &grubEnvironment{saved: "togi", values: map[string]string{tuningboot.CountVariable: value}}
			grub := GRUB{run: env.run}
			if got, err := grub.Get(tuningboot.CountVariable); err != nil || got != value {
				t.Fatalf("stored counter normalized: got=%q want=%q err=%v", got, value, err)
			}
			count, retry, err := tuningboot.RestartLimit(grub)
			if err == nil || retry || count != 0 || env.saved != "togi" || env.values[tuningboot.CountVariable] != value || len(env.values) != 1 {
				t.Fatalf("malformed count granted recovery: count=%d retry=%v values=%v saved=%q err=%v", count, retry, env.values, env.saved, err)
			}
			if diff := cmp.Diff([][]string{{"list"}, {"list"}}, env.operations); diff != "" {
				t.Fatalf("malformed count changed environment (-want +got):\n%s", diff)
			}
		})
	}
}

func TestGRUBRetryStateFailureWindows(t *testing.T) {
	// Exhaustion lists the count, sets the count and reason together, then
	// performs the existing read/unset/read saved-entry verification.
	for at := 1; at <= 5; at++ {
		for _, afterEffect := range []bool{false, true} {
			t.Run(fmt.Sprintf("operation %d after %v", at, afterEffect), func(t *testing.T) {
				env := &grubEnvironment{saved: "togi", values: map[string]string{tuningboot.CountVariable: "2"}, failAt: at, after: afterEffect}
				count, retry, err := tuningboot.RestartLimit(GRUB{run: env.run})
				if !errors.Is(err, errGRUBRunner) || retry {
					t.Fatalf("count=%d retry=%v error=%v", count, retry, err)
				}
				written := at > 2 || at == 2 && afterEffect
				if (env.values[tuningboot.ReasonVariable] != "") != written {
					t.Fatalf("reason write effect hidden: %+v", env.values)
				}
				unset := at > 4 || at == 4 && afterEffect
				if (env.saved == "") != unset {
					t.Fatalf("clear effect hidden: saved=%q", env.saved)
				}
			})
		}
	}
}

func TestGRUBReasonReadWriteUnsetFailures(t *testing.T) {
	for _, operation := range []string{"read", "write", "clear", "reset"} {
		for _, afterEffect := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s after %v", operation, afterEffect), func(t *testing.T) {
				env := &grubEnvironment{failAt: 1, after: afterEffect}
				grub := GRUB{run: env.run}
				var err error
				switch operation {
				case "read":
					_, err = tuningboot.ReadReason(grub)
				case "write":
					err = tuningboot.WriteReason(grub, tuningboot.Reason{ID: "test", Count: 1, Reason: "retry"})
				case "clear":
					env.values = map[string]string{tuningboot.ReasonVariable: "record"}
					err = tuningboot.ClearReason(grub)
				case "reset":
					env.values = map[string]string{tuningboot.CountVariable: "2"}
					err = tuningboot.ResetCount(grub)
				}
				if !errors.Is(err, errGRUBRunner) {
					t.Fatalf("failure not preserved: %v", err)
				}
			})
		}
	}
}
