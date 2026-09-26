package smu

import (
	"fmt"
	"math/bits"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"

	"github.com/shgew/shycler/internal/machine"
)

type location struct{ ccd, slot uint32 }

type Driver struct {
	root          string
	mb            Mailbox
	cores         []machine.CoreInfo
	slots         map[int]location
	mappingDetail string
	mappingErr    error
}

func Open(root string, mb Mailbox) (*Driver, error) {
	cores, err := topology(root)
	if err != nil {
		return nil, fmt.Errorf("read CPU topology: %w", err)
	}
	d := &Driver{root: root, mb: mb, cores: cores, slots: make(map[int]location, len(cores))}
	d.mapSlots()
	return d, nil
}

func (d *Driver) Topology() []machine.CoreInfo {
	cores := make([]machine.CoreInfo, len(d.cores))
	for i, core := range d.cores {
		cores[i] = core
		cores[i].CPUs = slices.Clone(core.CPUs)
	}
	return cores
}

func (d *Driver) Offset(core int) (int, error) {
	loc, err := d.location(core)
	if err != nil {
		return 0, err
	}
	response, err := d.mb.Command(0xd5, [6]uint32{loc.ccd<<28 | loc.slot<<20})
	if err != nil {
		return 0, fmt.Errorf("read core %02d offset: %w", core, err)
	}
	return int(int16(response[0] & 0xffff)), nil
}

func (d *Driver) SetOffset(core, offset int) error {
	loc, err := d.location(core)
	if err != nil {
		return err
	}
	arg := loc.ccd<<28 | loc.slot<<20 | uint32(uint16(int16(machine.ClampOffset(offset))))
	if _, err := d.mb.Command(0x06, [6]uint32{arg}); err != nil {
		return fmt.Errorf("set core %02d offset: %w", core, err)
	}
	return nil
}

func (d *Driver) SetAllOffsets(offset int) error {
	if d.mb == nil {
		return fmt.Errorf("set all offsets: ryzen_smu is not loaded")
	}
	arg := uint32(uint16(int16(machine.ClampOffset(offset))))
	if _, err := d.mb.Command(0x07, [6]uint32{arg}); err != nil {
		return fmt.Errorf("set all offsets: %w", err)
	}
	return nil
}

func (d *Driver) location(core int) (location, error) {
	if d.mappingErr != nil {
		return location{}, fmt.Errorf("per-core access refused: %w", d.mappingErr)
	}
	loc, ok := d.slots[core]
	if !ok {
		return location{}, fmt.Errorf("core %02d not found in topology", core)
	}
	return loc, nil
}

func (d *Driver) mapSlots() {
	if d.mb == nil {
		d.mappingErr = fmt.Errorf("ryzen_smu is not loaded")
		return
	}
	byCCD := make(map[int][]int)
	for _, core := range d.cores {
		byCCD[core.CCD] = append(byCCD[core.CCD], core.Core)
	}
	ccds := make([]int, 0, len(byCCD))
	for ccd := range byCCD {
		ccds = append(ccds, ccd)
	}
	slices.Sort(ccds)
	var details []string
	for _, ccd := range ccds {
		const rsmuResponse = 0x03b10570
		const rsmuCommand = 0x03b10524
		addr := 0x304a03dc + uint32(ccd)<<25
		firstProbe, err := d.mb.ReadSMN(rsmuResponse)
		if err != nil {
			d.mappingErr = fmt.Errorf("read RSMU response before CCD%d fuse: %w", ccd, err)
			return
		}
		fuse, err := d.mb.ReadSMN(addr)
		if err != nil {
			d.mappingErr = fmt.Errorf("read CCD%d fuse: %w", ccd, err)
			return
		}
		secondProbe, err := d.mb.ReadSMN(rsmuCommand)
		if err != nil {
			d.mappingErr = fmt.Errorf("read RSMU command before CCD%d fuse: %w", ccd, err)
			return
		}
		repeatedFuse, err := d.mb.ReadSMN(addr)
		if err != nil {
			d.mappingErr = fmt.Errorf("repeat CCD%d fuse read: %w", ccd, err)
			return
		}
		if firstProbe == secondProbe {
			d.mappingErr = fmt.Errorf("CCD%d fuse reads cannot be verified: RSMU probes both returned 0x%x", ccd, firstProbe)
			return
		}
		if fuse != repeatedFuse {
			d.mappingErr = fmt.Errorf("CCD%d fuse reads disagree: 0x%x and 0x%x", ccd, fuse, repeatedFuse)
			return
		}
		disabled := uint8(fuse)
		cores := byCCD[ccd]
		if live := 8 - bits.OnesCount8(disabled); live != len(cores) {
			d.mappingErr = fmt.Errorf("CCD%d fuse 0x%02x leaves %d live slots for %d cores", ccd, disabled, live, len(cores))
			return
		}
		var slots []int
		for slot := range 8 {
			if disabled&(1<<slot) == 0 {
				d.slots[cores[len(slots)]] = location{uint32(ccd), uint32(slot)}
				slots = append(slots, slot)
			}
		}
		details = append(details, fmt.Sprintf("CCD%d fuse 0x%02x: cores %02d-%02d on slots %d-%d", ccd, disabled, cores[0], cores[len(cores)-1], slots[0], slots[len(slots)-1]))
	}
	d.mappingDetail = strings.Join(details, "; ")
}

func topology(root string) ([]machine.CoreInfo, error) {
	base := filepath.Join(root, "sys/devices/system/cpu")
	entries, err := os.ReadDir(base)
	if err != nil {
		return nil, fmt.Errorf("list %s: %w", base, err)
	}
	byCore := make(map[int]*machine.CoreInfo)
	for _, entry := range entries {
		name := entry.Name()
		if !entry.IsDir() || !strings.HasPrefix(name, "cpu") {
			continue
		}
		cpu, err := strconv.Atoi(strings.TrimPrefix(name, "cpu"))
		if err != nil || cpu < 0 {
			continue
		}
		cpuDir := filepath.Join(base, name)
		online, err := os.ReadFile(filepath.Join(cpuDir, "online"))
		if err != nil && !os.IsNotExist(err) {
			return nil, fmt.Errorf("read %s online: %w", name, err)
		}
		if strings.TrimSpace(string(online)) == "0" {
			continue
		}
		id, err := readInt(filepath.Join(cpuDir, "topology/core_id"))
		if err != nil {
			return nil, fmt.Errorf("read %s core id: %w", name, err)
		}
		core := byCore[id]
		if core == nil {
			core = &machine.CoreInfo{Core: id}
			byCore[id] = core
		}
		core.CPUs = append(core.CPUs, cpu)
	}
	if len(byCore) == 0 {
		return nil, fmt.Errorf("no online CPU cores in %s", base)
	}
	cores := make([]machine.CoreInfo, 0, len(byCore))
	for _, core := range byCore {
		slices.Sort(core.CPUs)
		cores = append(cores, *core)
	}
	slices.SortFunc(cores, func(a, b machine.CoreInfo) int { return a.Core - b.Core })
	keys := make([]int, len(cores))
	cacheIDs := true
	for i, core := range cores {
		path := filepath.Join(base, fmt.Sprintf("cpu%d/cache/index3/id", core.CPUs[0]))
		key, err := readInt(path)
		if os.IsNotExist(err) {
			cacheIDs = false
			break
		}
		if err != nil {
			return nil, fmt.Errorf("read CCD cache id for core %02d: %w", core.Core, err)
		}
		keys[i] = key
	}
	if !cacheIDs {
		for i, core := range cores {
			path := filepath.Join(base, fmt.Sprintf("cpu%d/topology/die_id", core.CPUs[0]))
			key, err := readInt(path)
			if err != nil {
				return nil, fmt.Errorf("read die id for core %02d: %w", core.Core, err)
			}
			keys[i] = key
		}
	}
	ordered := slices.Clone(keys)
	slices.Sort(ordered)
	ordered = slices.Compact(ordered)
	for i := range cores {
		cores[i].CCD, _ = slices.BinarySearch(ordered, keys[i])
	}
	return cores, nil
}

func readInt(path string) (int, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return 0, err
	}
	value, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil {
		return 0, fmt.Errorf("parse %s: %w", path, err)
	}
	return value, nil
}
