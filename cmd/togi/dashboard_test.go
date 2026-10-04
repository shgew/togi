package main

import (
	"context"
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

func TestDashboardPanicFallsBackToEventLines(t *testing.T) {
	t.Parallel()
	out, err := os.CreateTemp(t.TempDir(), "events")
	if err != nil {
		t.Fatal(err)
	}
	defer out.Close()
	d := dashboard{out: out, run: func(context.Context, string, *os.File) error { panic("projection bug") }}
	d.show()
	d.hide()
	if _, err := d.Write([]byte("next event\n")); err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(out.Name())
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff("togi run: dashboard: panic: projection bug; printing events instead\nnext event\n", string(got)); diff != "" {
		t.Fatalf("dashboard panic fallback: %s", diff)
	}
}
