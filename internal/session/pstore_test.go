package session

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/render"
)

type pstoreKernel struct {
	machine.Kernel
	record *machine.PstoreRecord
	err    error
}

func (k pstoreKernel) SavedPstore(string) (*machine.PstoreRecord, error) { return k.record, k.err }

func TestCrashRecoveryPstore(t *testing.T) {
	t.Parallel()
	record := &machine.PstoreRecord{Path: "/var/lib/systemd/pstore/1791027456/001/dmesg.txt", Lines: []string{strings.Repeat("x", 4000), "kernel diagnostic\x1b[2J"}}
	for _, tc := range []struct {
		name    string
		record  *machine.PstoreRecord
		err     error
		warning bool
	}{
		{name: "saved record", record: record},
		{name: "no record"},
		{name: "unreadable archive", err: errors.New("read pstore record: permission denied"), warning: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r, k, _ := boundaryRunner(t)
			oldBoot := r.in.Boot
			r.in.Machine.Clock.(interface{ Reboot() }).Reboot()
			r.in.Boot, _ = r.in.Machine.Host.BootID()
			r = reopenBoundaryRunner(t, r, k)
			r.in.Machine.Kernel = pstoreKernel{Kernel: r.in.Machine.Kernel, record: tc.record, err: tc.err}
			if err := r.recoverCrashes(context.Background()); err != nil {
				t.Fatal(err)
			}
			crash, ok := crashDetectedFor(r.in.Journal.Events(), oldBoot)
			if !ok {
				t.Fatal("recovery did not record crash")
			}
			if diff := cmp.Diff(tc.record, crash.Data.(*journal.CrashDetected).Pstore); diff != "" {
				t.Fatal(diff)
			}
			warnings := 0
			for _, e := range r.in.Journal.Events() {
				if p, ok := e.Data.(*journal.SessionWarning); ok && p.Operation == "read saved pstore" {
					warnings++
					if diff := cmp.Diff(tc.err.Error(), p.Error); diff != "" {
						t.Fatal(diff)
					}
				}
			}
			if diff := cmp.Diff(map[bool]int{false: 0, true: 1}[tc.warning], warnings); diff != "" {
				t.Fatal(diff)
			}
			before := len(r.in.Journal.Events())
			if err := r.recoverCrashes(context.Background()); err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(before, len(r.in.Journal.Events())); diff != "" {
				t.Fatal("repeat recovery: " + diff)
			}
			if tc.record != nil {
				t.Log(render.FormatLine(crash, r.in.Machine.Clock.Now().Location()))
				t.Logf("pstore=%s; tail bytes=%d; lines=%d", tc.record.Path, len(strings.Join(tc.record.Lines, "\n")), len(tc.record.Lines))
			} else {
				t.Logf("pstore absent; warnings=%d; recovery completed", warnings)
			}
		})
	}
}
