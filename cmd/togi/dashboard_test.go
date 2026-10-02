package main

import (
	"os"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestDashboardEventVisibility(t *testing.T) {
	t.Parallel()
	out, err := os.CreateTemp(t.TempDir(), "events")
	if err != nil {
		t.Fatal(err)
	}
	defer out.Close()
	d := dashboard{out: out}
	for _, tc := range []struct {
		showing bool
		line    string
	}{{false, "startup\n"}, {true, "hidden event\n"}, {false, "shutdown\n"}} {
		d.showing = tc.showing
		if n, err := d.Write([]byte(tc.line)); n != len(tc.line) || err != nil {
			t.Fatalf("write %d, %v", n, err)
		}
	}
	got, err := os.ReadFile(out.Name())
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff("startup\nshutdown\n", string(got)); diff != "" {
		t.Fatalf("dashboard event visibility: %s", diff)
	}
}
