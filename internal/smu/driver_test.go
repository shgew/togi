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

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
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
	put(t, root, "proc/cpuinfo", "cpu family : 26\nmodel : 68\nmodel name : Zen Test\n")
	put(t, root, "sys/kernel/ryzen_smu_drv/codename", "23\n")
	put(t, root, "sys/kernel/ryzen_smu_drv/drv_version", "0.1\n")
	put(t, root, "sys/kernel/ryzen_smu_drv/version", "57.13\n")
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
		name  string
		cores int
		fuse  uint32
		ccd   int
		cache bool
		valid bool
	}{
		{"full-cache-topology", 8, 0x00, 0, true, true},
		{"full-die-fallback", 8, 0x00, 0, false, true},
		{"full-fuse-upper-bits", 8, 0x100, 0, true, true},
		{"first-slot-disabled", 7, 0x01, 0, true, false},
		{"last-slot-disabled", 7, 0x80, 0, true, false},
		{"harvested-die-fallback", 6, 0x81, 0, false, false},
		{"interior-slots-disabled", 6, 0x42, 0, true, false},
		{"all-slots-disabled", 8, 0xff, 0, true, false},
		{"second-ccd-disabled", 8, 0x81, 1, true, false},
		{"fuse-topology-mismatch", 8, 0x01, 0, true, false},
		{"full-fuse-missing-cores", 6, 0x00, 0, true, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root, mb := fixture(t, tc.cores, tc.cache)
			mb.fuses[0x304a03dc+uint32(tc.ccd)<<25] = tc.fuse
			if tc.cores != 8 {
				mb.fuses[0x304a03dc+1<<25] = tc.fuse
			}
			d, err := Open(root, mb)
			if err != nil {
				t.Fatal(err)
			}
			check := d.CheckSlotMapping()
			if check.OK != tc.valid {
				t.Fatalf("slot mapping: %+v", check)
			}
			if !tc.valid {
				if uint8(tc.fuse) != 0 {
					if !strings.Contains(check.Detail, fmt.Sprintf("CCD%d", tc.ccd)) ||
						!strings.Contains(check.Detail, fmt.Sprintf("0x%02x", uint8(tc.fuse))) ||
						!strings.Contains(check.Detail, "harvested CCDs are not yet supported") {
						t.Fatalf("refusal omits CCD, fuse or support limit: %+v", check)
					}
				}
				for _, core := range d.Topology() {
					if _, err := d.Offset(core.Core); err == nil {
						t.Fatalf("per-core read accepted for core %d", core.Core)
					}
					if err := d.SetOffset(core.Core, -10); err == nil {
						t.Fatalf("per-core write accepted for core %d", core.Core)
					}
				}
				if diff := cmp.Diff([]call(nil), mb.commands, cmp.AllowUnexported(call{})); diff != "" {
					t.Fatalf("commands issued with unsupported slot mapping (-want +got):\n%s", diff)
				}
				return
			}
			for _, core := range d.Topology() {
				if err := d.SetOffset(core.Core, -10); err != nil {
					t.Fatal(err)
				}
				want := call{0x06, uint32(core.CCD)<<28 | uint32(core.Core%8)<<20 | 0xfff6}
				if diff := cmp.Diff(want, mb.commands[len(mb.commands)-1], cmp.AllowUnexported(call{})); diff != "" {
					t.Fatalf("core %d encoding (-want +got):\n%s", core.Core, diff)
				}
			}
		})
	}
}

func TestSlotIdentityPreflight(t *testing.T) {
	for _, tc := range []struct {
		name   string
		cores  [2][8]int
		cache  bool
		detail string
	}{
		{
			name:  "full-topology",
			cores: [2][8]int{{0, 1, 2, 3, 4, 5, 6, 7}, {8, 9, 10, 11, 12, 13, 14, 15}},
			cache: true,
		},
		{
			name:   "cross-ccd-ids",
			cores:  [2][8]int{{0, 1, 2, 3, 4, 5, 6, 8}, {7, 9, 10, 11, 12, 13, 14, 15}},
			cache:  true,
			detail: "CCD0 core IDs disagree with modulo-eight slots: core 08 at slot 7, want slot 0",
		},
		{
			name:   "multiple-offending-ids",
			cores:  [2][8]int{{0, 1, 2, 3, 4, 5, 8, 9}, {6, 7, 10, 11, 12, 13, 14, 15}},
			cache:  true,
			detail: "CCD0 core IDs disagree with modulo-eight slots: core 08 at slot 6, want slot 0; core 09 at slot 7, want slot 1",
		},
		{
			name:   "second-ccd-id",
			cores:  [2][8]int{{0, 1, 2, 3, 4, 5, 6, 7}, {8, 9, 10, 11, 12, 13, 14, 16}},
			cache:  true,
			detail: "CCD1 core IDs disagree with modulo-eight slots: core 16 at slot 7, want slot 0",
		},
		{
			name:   "cross-ccd-die-fallback",
			cores:  [2][8]int{{0, 1, 2, 3, 4, 5, 6, 8}, {7, 9, 10, 11, 12, 13, 14, 15}},
			detail: "CCD0 core IDs disagree with modulo-eight slots: core 08 at slot 7, want slot 0",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root, mb := fixture(t, 8, tc.cache)
			for ccd, cores := range tc.cores {
				for slot, core := range cores {
					cpuID := ccd*8 + slot
					cpu(t, root, cpuID, core, ccd, tc.cache)
					cpu(t, root, cpuID+16, core, ccd, tc.cache)
				}
			}
			d, err := Open(root, mb)
			if err != nil {
				t.Fatal(err)
			}
			valid := tc.detail == ""
			if diff := cmp.Diff(valid, d.CheckSlotMapping().OK); diff != "" {
				t.Fatalf("slot mapping accepted mismatched identity (-want +got):\n%s", diff)
			}
			if valid {
				if check := d.CheckReadback(); !check.OK {
					t.Fatalf("correct full topology refused: %+v", check)
				}
				want := make([]call, 0, 16)
				for _, core := range d.Topology() {
					want = append(want, call{0xd5, uint32(core.CCD)<<28 | uint32(core.Core%8)<<20})
				}
				if diff := cmp.Diff(want, mb.commands, cmp.AllowUnexported(call{})); diff != "" {
					t.Fatalf("readback slots (-want +got):\n%s", diff)
				}
				return
			}
			if diff := cmp.Diff(tc.detail, d.CheckSlotMapping().Detail); diff != "" {
				t.Fatalf("slot refusal detail (-want +got):\n%s", diff)
			}
			if check := d.CheckReadback(); check.OK || !strings.Contains(check.Detail, tc.detail) {
				t.Fatalf("readback bypassed slot refusal: %+v", check)
			}
			for _, core := range d.Topology() {
				if _, err := d.Offset(core.Core); err == nil {
					t.Fatalf("per-core read accepted for core %d", core.Core)
				}
				if err := d.SetOffset(core.Core, -10); err == nil {
					t.Fatalf("per-core write accepted for core %d", core.Core)
				}
			}
			if diff := cmp.Diff([]call(nil), mb.commands, cmp.AllowUnexported(call{})); diff != "" {
				t.Fatalf("commands issued with mismatched slot identity (-want +got):\n%s", diff)
			}
			t.Logf("slot mapping preflight: OK=false detail=%s; no per-core commands", d.CheckSlotMapping().Detail)
		})
	}
}

func TestStaleCCDFuseRefusesPerCoreAccess(t *testing.T) {
	for _, failRead := range [][]int{{1}, {2}, {1, 2}} {
		t.Run(fmt.Sprint(failRead), func(t *testing.T) {
			root, mb := fixture(t, 8, true)
			const ccd0 = 0x304a03dc
			const ccd1 = ccd0 + 1<<25
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
		{"response", 0x00, 0x6e, true},
		{"command", 0x01, 0x00, true},
		{"indistinguishable", 0x00, 0x00, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root, mb := fixture(t, 8, true)
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
	if check := d.CheckDriver(); check.OK || !strings.Contains(check.Detail, "ryzen_smu is not loaded") {
		t.Fatalf("missing driver check: %+v", check)
	}
	if err := d.SetAllOffsets(0); err == nil {
		t.Fatal("missing driver accepted all-core command")
	}
	if _, err := Open(t.TempDir(), nil); err == nil {
		t.Fatal("missing topology accepted")
	}
}

func TestConstructionRefusesUnsupportedIdentityWithoutAccess(t *testing.T) {
	for _, tc := range []struct {
		name, path, value string
	}{
		{"family", "proc/cpuinfo", "cpu family : 25\nmodel : 68\n"},
		{"model", "proc/cpuinfo", "cpu family : 26\nmodel : 80\n"},
		{"codename", "sys/kernel/ryzen_smu_drv/codename", "22\n"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root, mb := fixture(t, 8, true)
			put(t, root, tc.path, tc.value)
			mb.smnRead = func(uint32) (uint32, error) {
				t.Fatal("SMN read before identity validation")
				return 0, nil
			}
			d, err := Open(root, mb)
			if err != nil {
				t.Fatal(err)
			}
			if err := d.ValidateSMU(); err == nil {
				t.Fatal("unsupported identity accepted")
			}
			if len(d.Topology()) != 16 {
				t.Fatal("unsupported identity lost topology introspection")
			}
			if _, err := d.Offset(0); err == nil {
				t.Fatal("unsupported offset read accepted")
			}
			if err := d.SetOffset(0, -10); err == nil {
				t.Fatal("unsupported per-core write accepted")
			}
			if err := d.SetAllOffsets(0); err == nil {
				t.Fatal("unsupported emergency zero accepted")
			}
			if _, err := d.BIOSContext(); err == nil {
				t.Fatal("unsupported BIOS read accepted")
			}
			if len(mb.commands) != 0 {
				t.Fatalf("mailbox commands before validation: %v", mb.commands)
			}
		})
	}
}

func TestUnvalidatedDriverRefusesEveryAccess(t *testing.T) {
	mb := &fakeMailbox{}
	mb.smnRead = func(uint32) (uint32, error) {
		t.Fatal("unvalidated SMN read")
		return 0, nil
	}
	d := &Driver{mb: mb}
	d.mapSlots()
	if d.CheckSlotMapping().OK {
		t.Fatal("unvalidated slot mapping accepted")
	}
	if _, err := d.Offset(0); err == nil {
		t.Fatal("unvalidated offset read accepted")
	}
	if err := d.SetOffset(0, -10); err == nil {
		t.Fatal("unvalidated per-core write accepted")
	}
	if err := d.SetAllOffsets(0); err == nil {
		t.Fatal("unvalidated all-core zero accepted")
	}
	if _, err := d.BIOSContext(); err == nil {
		t.Fatal("unvalidated BIOS context read accepted")
	}
	if len(mb.commands) != 0 {
		t.Fatalf("unvalidated mailbox commands: %v", mb.commands)
	}
}

func TestOpenIdentityChecksWithoutMailbox(t *testing.T) {
	root, mb := fixture(t, 8, true)
	mb.smnRead = func(addr uint32) (uint32, error) {
		t.Fatalf("identity driver read SMN 0x%x", addr)
		return 0, nil
	}
	d, err := OpenIdentity(root, mb)
	if err != nil {
		t.Fatal(err)
	}
	for _, check := range []machine.Check{d.CheckCPU(), d.CheckDriver()} {
		if !check.OK {
			t.Fatalf("identity check %s failed: %s", check.Name, check.Detail)
		}
	}
	if check := d.CheckSlotMapping(); check.OK {
		t.Fatalf("identity driver mapped slots: %+v", check)
	}
	if _, err := d.Offset(0); err == nil {
		t.Fatal("identity driver read an offset")
	}
	if err := d.SetOffset(0, -10); err == nil {
		t.Fatal("identity driver wrote a per-core offset")
	}
	if err := d.SetAllOffsets(0); err == nil {
		t.Fatal("identity driver wrote all offsets")
	}
	if _, err := d.BIOSContext(); err == nil {
		t.Fatal("identity driver read the BIOS context")
	}
	if len(mb.commands) != 0 {
		t.Fatalf("identity driver mailbox commands: %v", mb.commands)
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

func TestInvalidIdentityFilesPreventHardwareAccess(t *testing.T) {
	for _, tc := range []struct {
		name, path, text string
		missing          bool
	}{
		{"missing CPU", "proc/cpuinfo", "", true},
		{"malformed family", "proc/cpuinfo", "cpu family: invalid\nmodel: 68\n", false},
		{"malformed model", "proc/cpuinfo", "cpu family: 26\nmodel: invalid\n", false},
		{"missing codename", "sys/kernel/ryzen_smu_drv/codename", "", true},
		{"missing driver version", "sys/kernel/ryzen_smu_drv/drv_version", "", true},
		{"missing SMU version", "sys/kernel/ryzen_smu_drv/version", "", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root, mb := fixture(t, 8, true)
			if tc.missing {
				if err := os.Remove(filepath.Join(root, tc.path)); err != nil {
					t.Fatal(err)
				}
			} else {
				put(t, root, tc.path, tc.text)
			}
			mb.smnRead = func(uint32) (uint32, error) { t.Fatal("identity refusal accessed SMN"); return 0, nil }
			d, err := Open(root, mb)
			if err != nil {
				t.Fatal(err)
			}
			if err := d.ValidateSMU(); err == nil {
				t.Fatal("invalid identity validated")
			}
			check := d.CheckCPU()
			if tc.path != "proc/cpuinfo" {
				check = d.CheckDriver()
			}
			wantDetail := "parse cpu family"
			if tc.missing {
				wantDetail = "read " + filepath.Join(root, tc.path) + ":"
			}
			if check.OK || !strings.HasPrefix(check.Detail, wantDetail) {
				t.Fatalf("identity refusal diagnostic = %+v, want %q", check, wantDetail)
			}
			if err := d.SetOffset(0, -10); err == nil {
				t.Fatal("invalid identity allowed write")
			}
			if err := d.SetAllOffsets(0); err == nil {
				t.Fatal("invalid identity allowed emergency write")
			}
			if diff := cmp.Diff([]call(nil), mb.commands); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestMailboxWriteFailureIsReported(t *testing.T) {
	root, mb := fixture(t, 8, true)
	d, err := Open(root, mb)
	if err != nil {
		t.Fatal(err)
	}
	rejected := errors.New("mailbox rejected write")
	mb.err = rejected
	for _, tc := range []struct {
		name  string
		write func() error
		want  call
	}{
		{"per core", func() error { return d.SetOffset(9, -60) }, call{0x06, 0x1010ffce}},
		{"all cores", func() error { return d.SetAllOffsets(10) }, call{0x07, 0}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.write()
			if !errors.Is(err, rejected) || !strings.Contains(err.Error(), "set") {
				t.Fatalf("write failure lost: %v", err)
			}
			if diff := cmp.Diff(tc.want, mb.commands[len(mb.commands)-1], cmp.AllowUnexported(call{})); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestFuseReadFailureRefusesPerCoreAccess(t *testing.T) {
	for failed := 1; failed <= 4; failed++ {
		t.Run(fmt.Sprint(failed), func(t *testing.T) {
			root, mb := fixture(t, 8, true)
			reads := 0
			unavailable := errors.New("SMN unavailable")
			mb.smnRead = func(addr uint32) (uint32, error) {
				reads++
				if reads == failed {
					return 0, unavailable
				}
				return mb.fuses[addr], nil
			}
			d, err := Open(root, mb)
			if err != nil {
				t.Fatal(err)
			}
			if check := d.CheckSlotMapping(); check.OK || !strings.Contains(check.Detail, unavailable.Error()) {
				t.Fatalf("mapping failure: %+v", check)
			}
			if _, err := d.Offset(0); !errors.Is(err, unavailable) {
				t.Fatalf("read accepted or cause lost: %v", err)
			}
			if err := d.SetOffset(0, -10); !errors.Is(err, unavailable) {
				t.Fatalf("write accepted or cause lost: %v", err)
			}
			if diff := cmp.Diff([]call(nil), mb.commands); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestTopologyRejectsMissingOrMalformedIdentity(t *testing.T) {
	for _, path := range []string{"topology/core_id", "cache/index3/id", "topology/die_id", "online"} {
		t.Run(path, func(t *testing.T) {
			root, mb := fixture(t, 8, path != "topology/die_id")
			full := filepath.Join(root, "sys/devices/system/cpu/cpu0", path)
			if path == "online" {
				if err := os.Mkdir(full, 0755); err != nil {
					t.Fatal(err)
				}
			} else {
				put(t, root, "sys/devices/system/cpu/cpu0/"+path, "invalid\n")
			}
			d, err := Open(root, mb)
			if err == nil || d != nil || !strings.Contains(err.Error(), full) {
				t.Fatalf("invalid topology accepted: %v, %v", d, err)
			}
			if diff := cmp.Diff([]call(nil), mb.commands); diff != "" {
				t.Fatal(diff)
			}
		})
	}
	root := t.TempDir()
	put(t, root, "sys/devices/system/cpu/notcpu", "ignored")
	put(t, root, "sys/devices/system/cpu/cpunotnumber/topology/core_id", "0")
	if _, err := topology(root); err == nil || !strings.Contains(err.Error(), "no online CPU cores") {
		t.Fatalf("empty topology: %v", err)
	}
}

func TestBIOSContextReadFailures(t *testing.T) {
	for _, path := range []string{"sys/class/dmi/id/bios_version", "sys/class/dmi/id/board_vendor", "sys/class/dmi/id/board_name", "proc/cpuinfo", "boost"} {
		t.Run(path, func(t *testing.T) {
			root, mb := fixture(t, 8, true)
			for _, name := range []string{"bios_version", "board_vendor", "board_name"} {
				put(t, root, "sys/class/dmi/id/"+name, name)
			}
			d, err := Open(root, mb)
			if err != nil {
				t.Fatal(err)
			}
			cause := errors.New("boost unavailable")
			if path == "boost" {
				mb.err = cause
			} else {
				if err := os.Remove(filepath.Join(root, path)); err != nil {
					t.Fatal(err)
				}
				cause = os.ErrNotExist
			}
			_, err = d.BIOSContext()
			if !errors.Is(err, cause) {
				t.Fatalf("BIOS read failure lost: %v", err)
			}
			if path != "boost" && len(mb.commands) != 0 {
				t.Fatalf("boost read after missing context: %v", mb.commands)
			}
		})
	}
}

func TestSysfsRejectsUnavailableMailboxFiles(t *testing.T) {
	for _, name := range []string{"smu_args", "rsmu_cmd", "smn"} {
		t.Run(name, func(t *testing.T) {
			root := t.TempDir()
			put(t, root, "sys/kernel/ryzen_smu_drv/smu_args", strings.Repeat("\x00", 24))
			put(t, root, "sys/kernel/ryzen_smu_drv/rsmu_cmd", strings.Repeat("\x00", 4))
			put(t, root, "sys/kernel/ryzen_smu_drv/smn", strings.Repeat("\x00", 4))
			if err := os.Remove(filepath.Join(root, "sys/kernel/ryzen_smu_drv", name)); err != nil {
				t.Fatal(err)
			}
			mb := Sysfs(root)
			var err error
			if name == "smn" {
				_, err = mb.ReadSMN(0)
			} else {
				_, err = mb.Command(1, [6]uint32{})
			}
			want := "open " + filepath.Join(root, "sys/kernel/ryzen_smu_drv", name) + ":"
			if !errors.Is(err, os.ErrNotExist) || !strings.HasPrefix(err.Error(), want) {
				t.Fatalf("mailbox file failure: %v, want write-stage %q", err, want)
			}
			if name == "smu_args" {
				command, err := os.ReadFile(filepath.Join(root, "sys/kernel/ryzen_smu_drv/rsmu_cmd"))
				if err != nil {
					t.Fatal(err)
				}
				if diff := cmp.Diff(make([]byte, 4), command); diff != "" {
					t.Fatalf("command issued after failed argument write (-want +got):\n%s", diff)
				}
			}
		})
	}
	root := t.TempDir()
	put(t, root, "sys/kernel/ryzen_smu_drv/smu_args", strings.Repeat("\x00", 24))
	put(t, root, "sys/kernel/ryzen_smu_drv/rsmu_cmd", "")
	if _, err := Sysfs(root).Command(0x42, [6]uint32{}); !strings.Contains(fmt.Sprint(err), "0x42 (unknown status)") {
		t.Fatalf("unknown status lost: %v", err)
	}
}
