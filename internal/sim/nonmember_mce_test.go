package sim

import (
	"errors"
	"fmt"
	"maps"
	"slices"
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

// TestJointCrashNonmemberMCE pins a loaded joint crash whose next boot reports an uncorrected load-store MCE on a
// core that is neither a joint member nor loaded by the trial.
func TestJointCrashNonmemberMCE(t *testing.T) {
	const cores = 6
	work := machine.PickWorkload(machine.R1, 0)
	members := map[int]int{2: -20, 3: -20}
	loaded := []int{2, 3}
	config := func(joints []Joint) Config {
		bios := make([]int, cores)
		for core, offset := range members {
			bios[core] = offset
		}
		// Every model-drawn crash would carry an MCE on its own loaded core, so a named nonmember core can only
		// come from the joint.
		model := sharp(machine.Crash)
		model.CrashMCE = 1
		return Config{Cores: cores, BIOS: bios, Limits: flat(cores, -30, -30), Model: model, Joints: joints}
	}
	for _, named := range []int{0, cores - 1} {
		t.Run(fmt.Sprintf("core %d", named), func(t *testing.T) {
			if _, member := members[named]; member || slices.Contains(loaded, named) {
				t.Fatalf("fixture core %d is a member %v or loaded %v", named, slices.Sorted(maps.Keys(members)), loaded)
			}
			control := newMachine(t, config(nil))
			if res, err := runSpec(t, control, "0001", machine.R1, work, loaded, time.Minute, nil); err != nil || res.Signal != "" {
				t.Fatalf("trial failed without the joint: %+v %v", res, err)
			}

			m := newMachine(t, config([]Joint{{Members: members, Rate: 1e6, Signal: machine.Crash, CrashMCECore: &named}}))
			crashed, err := m.Seams().Host.BootID()
			if err != nil {
				t.Fatal(err)
			}
			if _, err := runSpec(t, m, "0001", machine.R1, work, loaded, time.Minute, nil); !errors.Is(err, machine.ErrCrashed) {
				t.Fatalf("joint trial: %v, want %v", err, machine.ErrCrashed)
			}
			if _, err := m.Seams().Host.BootID(); !errors.Is(err, machine.ErrCrashed) {
				t.Fatalf("BootID after crash: %v", err)
			}
			m.Reboot()
			boot, err := m.Seams().Host.BootID()
			if err != nil || boot == crashed {
				t.Fatalf("boot after reboot %q (crashed %q): %v", boot, crashed, err)
			}
			if mces, err := m.Seams().Kernel.MCEs(crashed, 0); err != nil || len(mces) != 0 {
				t.Fatalf("crashed boot MCEs %+v %v", mces, err)
			}
			mces, err := m.Seams().Kernel.MCEs(boot, 0)
			if err != nil || len(mces) != 1 {
				t.Fatalf("boot MCEs %+v %v, want one", mces, err)
			}
			got := mces[0]
			if got.CPU != named || got.Core != named || got.BankType != machine.LoadStore || got.Corrected {
				t.Fatalf("boot MCE %+v, want uncorrected %s on core %d", got, machine.LoadStore, named)
			}
		})
	}
}
