package smu

import (
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"code.marleb.org/shgew/shycler/internal/machine"
	"github.com/google/go-cmp/cmp"
)

type call struct{ command, arg uint32 }
type fakeMailbox struct {
	fuses     map[uint32]uint32
	responses map[uint32][6]uint32
	commands  []call
	err       error
	smnRead   func(uint32) (uint32, error)
}

func (f *fakeMailbox) Command(cmd uint32, args [6]uint32) ([6]uint32, error) {
	f.commands = append(f.commands, call{cmd, args[0]})
	return f.responses[cmd], f.err
}

func (f *fakeMailbox) ReadSMN(addr uint32) (uint32, error) {
	if f.smnRead != nil {
		return f.smnRead(addr)
	}
	return f.fuses[addr], f.err
}

func put(t *testing.T, root, path, text string) {
	t.Helper()
	full := filepath.Join(root, path)
	if err := os.MkdirAll(filepath.Dir(full), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(full, []byte(text), 0644); err != nil {
		t.Fatal(err)
	}
}

func cpu(t *testing.T, root string, cpuID, coreID, ccd int, cache bool) {
	t.Helper()
	base := fmt.Sprintf("sys/devices/system/cpu/cpu%d/", cpuID)
	put(t, root, base+"topology/core_id", fmt.Sprint(coreID))
	put(t, root, base+"topology/die_id", fmt.Sprint(ccd+10))
	if cache {
		put(t, root, base+"cache/index3/id", fmt.Sprint(ccd+20))
	}
}

func fixture(t *testing.T, coresPerCCD int, cache bool) (string, *fakeMailbox) {
	t.Helper()
	root := t.TempDir()
	mb := &fakeMailbox{fuses: map[uint32]uint32{0x03b10570: 1, 0x03b10524: 0x6e}, responses: map[uint32][6]uint32{}}
	for ccd := range 2 {
		for index := range coresPerCCD {
			core := ccd*8 + index
			cpu(t, root, core, core, ccd, cache)
			cpu(t, root, core+16, core, ccd, cache)
		}
	}
	return root, mb
}

func TestTopologyAndEncoding(t *testing.T) {
	root, mb := fixture(t, 8, true)
	put(t, root, "sys/devices/system/cpu/cpu32/topology/core_id", "99")
	put(t, root, "sys/devices/system/cpu/cpu32/online", "0\n")
	d, err := Open(root, mb)
	if err != nil {
		t.Fatal(err)
	}
	got := d.Topology()
	if len(got) != 16 || got[9].CCD != 1 {
		t.Fatalf("topology: %+v", got)
	}
	if diff := cmp.Diff([]int{9, 25}, got[9].CPUs); diff != "" {
		t.Fatalf("core 9 cpus mismatch (-want +got):\n%s", diff)
	}
	if check := d.CheckSlotMapping(); !check.OK || check.Detail != "CCD0 fuse 0x00: cores 00-07 on slots 0-7; CCD1 fuse 0x00: cores 08-15 on slots 0-7" {
		t.Fatalf("mapping: %+v", check)
	}
	for _, tc := range []struct {
		core, offset int
		want         uint32
	}{
		{9, -12, 0x1010fff4}, {9, -60, 0x1010ffce}, {9, 5, 0x10100000},
	} {
		if err := d.SetOffset(tc.core, tc.offset); err != nil {
			t.Fatal(err)
		}
		if got := mb.commands[len(mb.commands)-1]; got != (call{0x06, tc.want}) {
			t.Fatalf("SetOffset(%d, %d): %+v, want %x", tc.core, tc.offset, got, tc.want)
		}
	}
	if err := d.SetAllOffsets(-60); err != nil {
		t.Fatal(err)
	}
	if got := mb.commands[len(mb.commands)-1]; got != (call{0x07, 0xffce}) {
		t.Fatalf("set all: %+v", got)
	}
	for _, tc := range []struct {
		raw  uint32
		want int
	}{{0xffec, -20}, {0x000a, 10}} {
		mb.responses[0xd5] = [6]uint32{tc.raw}
		got, err := d.Offset(9)
		if err != nil || got != tc.want {
			t.Fatalf("Offset(9) raw %x: %d, %v", tc.raw, got, err)
		}
		if last := mb.commands[len(mb.commands)-1]; last != (call{0xd5, 0x10100000}) {
			t.Fatalf("get core command: %+v", last)
		}
	}
	if err := d.SetOffset(999, 0); err == nil {
		t.Fatal("unknown core accepted")
	}
	d.Topology()[0].CPUs[0] = 100
	if d.Topology()[0].CPUs[0] != 0 {
		t.Fatal("topology returned internal CPU slice")
	}
}

func TestMappingAndFallback(t *testing.T) {
	for _, tc := range []struct {
		name, want string
		cores      int
		fuse       uint32
		cache      bool
		valid      bool
		wantSlot   uint32
	}{
		{"full-fuse-mismatch", "CCD0 fuse 0x01 leaves 7 live slots for 8 cores", 8, 0x01, true, false, 0},
		{"harvested-die-fallback", "CCD0 fuse 0x81: cores 00-05 on slots 1-6", 6, 0x81, false, true, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root, mb := fixture(t, tc.cores, tc.cache)
			mb.fuses[0x304a03dc] = tc.fuse
			if tc.cores == 6 {
				mb.fuses[0x304a03dc+(1<<25)] = 0x81
			}
			d, err := Open(root, mb)
			if err != nil {
				t.Fatal(err)
			}
			check := d.CheckSlotMapping()
			if check.OK != tc.valid || !strings.Contains(check.Detail, tc.want) {
				t.Fatalf("slot mapping: %+v", check)
			}
			if !tc.valid {
				if _, err := d.Offset(0); err == nil || !strings.Contains(err.Error(), "per-core access refused: "+tc.want) {
					t.Fatalf("get refused: %v", err)
				}
				if err := d.SetOffset(0, -10); err == nil || !strings.Contains(err.Error(), "per-core access refused: "+tc.want) {
					t.Fatalf("set refused: %v", err)
				}
				return
			}
			if got := d.Topology()[6].CCD; got != 1 {
				t.Fatalf("die id CCD rank: %d", got)
			}
			if err := d.SetOffset(0, -10); err != nil || mb.commands[0].arg>>20&7 != tc.wantSlot {
				t.Fatalf("harvested slot 1: %+v, %v", mb.commands, err)
			}
			if err := d.SetOffset(5, -10); err != nil || mb.commands[1].arg>>20&7 != 6 {
				t.Fatalf("harvested slot 6: %+v, %v", mb.commands, err)
			}
		})
	}
}

func TestStaleCCDFuseRefusesPerCoreAccess(t *testing.T) {
	for _, failRead := range [][]int{{1}, {2}, {1, 2}} {
		t.Run(fmt.Sprint(failRead), func(t *testing.T) {
			root, mb := fixture(t, 6, true)
			const ccd0 = 0x304a03dc
			const ccd1 = ccd0 + 1<<25
			mb.fuses[ccd0] = 0x81
			mb.fuses[ccd1] = 0x42
			var previous uint32
			var ccd1Reads int
			mb.smnRead = func(addr uint32) (uint32, error) {
				if addr == ccd1 {
					ccd1Reads++
					if slices.Contains(failRead, ccd1Reads) {
						return previous, nil
					}
				}
				previous = mb.fuses[addr]
				return previous, nil
			}
			d, err := Open(root, mb)
			if err != nil {
				t.Fatal(err)
			}
			if check := d.CheckSlotMapping(); check.OK {
				t.Fatalf("stale fuse accepted: %+v", check)
			}
			if _, err := d.Offset(8); err == nil {
				t.Fatal("per-core read accepted stale fuse")
			}
			if err := d.SetOffset(8, -10); err == nil {
				t.Fatal("per-core write accepted stale fuse")
			}
			if len(mb.commands) != 0 {
				t.Fatalf("commands issued with unverified slot mapping: %+v", mb.commands)
			}
		})
	}
}

func TestFuseEqualToProbeAcceptedWhenFresh(t *testing.T) {
	for _, tc := range []struct {
		name          string
		response, cmd uint32
		valid         bool
	}{
		{"response", 0x03, 0x6e, true},
		{"command", 0x01, 0x03, true},
		{"indistinguishable", 0x03, 0x03, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root, mb := fixture(t, 6, true)
			mb.fuses[0x304a03dc] = 0x03
			mb.fuses[0x304a03dc+1<<25] = 0x81
			mb.fuses[0x03b10570] = tc.response
			mb.fuses[0x03b10524] = tc.cmd
			d, err := Open(root, mb)
			if err != nil {
				t.Fatal(err)
			}
			if check := d.CheckSlotMapping(); check.OK != tc.valid {
				t.Fatalf("fresh fuse probe collision: %+v", check)
			}
		})
	}
}

func TestNoDriverAndTopologyError(t *testing.T) {
	root, _ := fixture(t, 8, true)
	d, err := Open(root, nil)
	if err != nil {
		t.Fatal(err)
	}
	if check := d.CheckDriver(); check.OK || check.Detail != "/sys/kernel/ryzen_smu_drv not found: ryzen_smu is not loaded" {
		t.Fatalf("driver: %+v", check)
	}
	if check := d.CheckSlotMapping(); check.OK || check.Detail != "ryzen_smu is not loaded" {
		t.Fatalf("mapping: %+v", check)
	}
	if _, err := d.Offset(0); err == nil {
		t.Fatal("per-core read with no driver accepted")
	}
	if err := d.SetAllOffsets(-10); err == nil {
		t.Fatal("set all with no driver accepted")
	}
	if _, err := Open(t.TempDir(), nil); err == nil {
		t.Fatal("missing topology accepted")
	}
}

func TestCPUDriverReadbackAndBIOS(t *testing.T) {
	root, mb := fixture(t, 8, true)
	put(t, root, "proc/cpuinfo", "processor : 0\ncpu family : 26\nmodel : 68\nmodel name : Zen Test\nmicrocode : 0xb404038\n\nprocessor : 1\nmodel name : not first\n")
	put(t, root, "sys/kernel/ryzen_smu_drv/codename", "23\n")
	put(t, root, "sys/kernel/ryzen_smu_drv/drv_version", "0.1\n")
	put(t, root, "sys/kernel/ryzen_smu_drv/version", "57.13\n")
	put(t, root, "sys/class/dmi/id/bios_version", "3.14\n")
	put(t, root, "sys/class/dmi/id/board_vendor", "ASRock\n")
	put(t, root, "sys/class/dmi/id/board_name", "X870E Taichi\n")
	mb.responses[0xd5] = [6]uint32{0xfff4}
	mb.responses[0x6e] = [6]uint32{5750}
	d, err := Open(root, mb)
	if err != nil {
		t.Fatal(err)
	}
	for _, check := range []machine.Check{d.CheckCPU(), d.CheckDriver(), d.CheckReadback()} {
		if !check.OK {
			t.Fatalf("check: %+v", check)
		}
	}
	if got := d.CheckCPU().Detail; got != "family 0x1A model 0x44 Zen Test" {
		t.Fatalf("CPU detail: %s", got)
	}
	if got := d.CheckDriver().Detail; got != "codename 23 (Granite Ridge), driver 0.1, SMU 57.13" {
		t.Fatalf("driver detail: %s", got)
	}
	if got := d.CheckReadback().Detail; !strings.HasPrefix(got, "16 cores read back: 00:-12 01:-12") {
		t.Fatalf("readback detail: %s", got)
	}
	bios, err := d.BIOSContext()
	if err != nil || bios != (machine.BIOSContext{BIOSVersion: "3.14", Board: "ASRock X870E Taichi", CPUModel: "Zen Test", Microcode: "0xb404038", BoostLimitMHz: 5750}) {
		t.Fatalf("BIOS context: %+v, %v", bios, err)
	}
	put(t, root, "sys/kernel/ryzen_smu_drv/codename", "22\n")
	if check := d.CheckDriver(); check.OK || check.Detail != "codename 22, want 23 (Granite Ridge)" {
		t.Fatalf("mismatched driver: %+v", check)
	}
	put(t, root, "proc/cpuinfo", "processor : 0\ncpu family : 25\nmodel : 68\nmodel name : Older CPU\n\n")
	if check := d.CheckCPU(); check.OK || !strings.HasSuffix(check.Detail, ": not Granite Ridge desktop (family 0x1A, model 0x40-0x4F)") {
		t.Fatalf("mismatched CPU: %+v", check)
	}
	mb.err = errors.New("SMU rejected request")
	if check := d.CheckReadback(); check.OK || !strings.Contains(check.Detail, "core 00: ") {
		t.Fatalf("readback error: %+v", check)
	}
}

func TestSysfsMailbox(t *testing.T) {
	root := t.TempDir()
	if Sysfs(root) != nil {
		t.Fatal("absent mailbox was not nil")
	}
	put(t, root, "sys/kernel/ryzen_smu_drv/smu_args", strings.Repeat("\x00", 24))
	put(t, root, "sys/kernel/ryzen_smu_drv/rsmu_cmd", strings.Repeat("\x00", 4))
	put(t, root, "sys/kernel/ryzen_smu_drv/smn", strings.Repeat("\x00", 4))
	mb := Sysfs(root)
	if _, err := mb.Command(0xff, [6]uint32{}); err == nil || !strings.Contains(err.Error(), "0xff (failed)") {
		t.Fatalf("status name: %v", err)
	}
	if _, err := mb.Command(0xfe, [6]uint32{}); err == nil || !strings.Contains(err.Error(), "0xfe (unknown command)") {
		t.Fatalf("status name: %v", err)
	}
	if _, err := mb.Command(0xfd, [6]uint32{}); err == nil || !strings.Contains(err.Error(), "0xfd (rejected: prerequisite)") {
		t.Fatalf("status name: %v", err)
	}
	if _, err := mb.Command(0xfc, [6]uint32{}); err == nil || !strings.Contains(err.Error(), "0xfc (rejected: busy)") {
		t.Fatalf("status name: %v", err)
	}
	args := [6]uint32{0x12345678, 4, 3, 2, 1, 0xffec}
	response, err := mb.Command(1, args)
	if err != nil || response != args {
		t.Fatalf("mailbox round trip: %v, %v", response, err)
	}
	data, err := os.ReadFile(filepath.Join(root, "sys/kernel/ryzen_smu_drv/smu_args"))
	if err != nil || len(data) != 24 || binary.LittleEndian.Uint32(data[:4]) != 0x12345678 {
		t.Fatalf("encoded arguments: %x, %v", data, err)
	}
	value, err := mb.ReadSMN(0x304a03dc)
	if err != nil || value != 0x304a03dc {
		t.Fatalf("SMN address: %x, %v", value, err)
	}
	put(t, root, "sys/kernel/ryzen_smu_drv/smn", "xxxxx")
	if _, err := mb.ReadSMN(0); err == nil || !strings.Contains(err.Error(), "got 5 bytes, want 4") {
		t.Fatalf("short SMN read: %v", err)
	}
}
