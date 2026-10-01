package sim

import (
	"errors"
	"fmt"
	"io"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestSMUInterruptionSurroundsActualEffect(t *testing.T) {
	for _, op := range []string{"set", "set_all", "read"} {
		for _, after := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s after %v", op, after), func(t *testing.T) {
				m := newMachine(t, Config{Seed: 1, Cores: 2, BIOS: []int{-5, -6}})
				if op == "read" {
					m.corruptPending[0] = true
				}
				fired := false
				m.InterruptSMU(func(access SMUOperation) error {
					if access.Op == op && access.After == after {
						fired = true
						return io.ErrClosedPipe
					}
					return nil
				})
				smu := m.Seams().SMU
				var err error
				switch op {
				case "set":
					err = smu.SetOffset(0, -17)
				case "set_all":
					err = smu.SetAllOffsets(-17)
				case "read":
					_, err = smu.Offset(0)
				}
				if !fired || !errors.Is(err, io.ErrClosedPipe) {
					t.Fatalf("hook fired %v, error %v", fired, err)
				}
				want := []int{-5, -6}
				if after && op == "set" {
					want[0] = -17
				}
				if after && op == "set_all" {
					want = []int{-17, -17}
				}
				if diff := cmp.Diff(want, m.regs); diff != "" {
					t.Fatalf("actual effect (-want +got):\n%s", diff)
				}
				if op == "read" && m.corruptPending[0] == after {
					t.Fatalf("readback observation consumed before=%v after=%v", m.corruptPending[0], after)
				}
			})
		}
	}
}
