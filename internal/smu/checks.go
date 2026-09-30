package smu

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/shgew/togi/internal/machine"
)

func (d *Driver) ValidateSMU() error {
	d.validated = false
	for _, check := range []machine.Check{d.CheckCPU(), d.CheckDriver()} {
		if !check.OK {
			return fmt.Errorf("validate %s: %s", check.Name, check.Detail)
		}
	}
	d.validated = true
	return nil
}

func (d *Driver) CheckCPU() machine.Check {
	check := machine.Check{Name: "cpu"}
	info, err := d.cpuInfo()
	if err != nil {
		check.Detail = err.Error()
		return check
	}
	family, familyErr := strconv.Atoi(info["cpu family"])
	model, modelErr := strconv.Atoi(info["model"])
	if familyErr != nil || modelErr != nil {
		check.Detail = fmt.Sprintf("parse cpu family %q and model %q: %v; %v", info["cpu family"], info["model"], familyErr, modelErr)
		return check
	}
	check.Detail = fmt.Sprintf("family 0x%X model 0x%X %s", family, model, info["model name"])
	check.OK = family == 0x1a && model >= 0x40 && model <= 0x4f
	if !check.OK {
		check.Detail += ": not Granite Ridge desktop (family 0x1A, model 0x40-0x4F)"
	}
	return check
}

func (d *Driver) CheckDriver() machine.Check {
	check := machine.Check{Name: "ryzen_smu"}
	if d.mb == nil {
		check.Detail = "/sys/kernel/ryzen_smu_drv not found: ryzen_smu is not loaded"
		return check
	}
	codename, err := d.trimmed("sys/kernel/ryzen_smu_drv/codename")
	if err != nil {
		check.Detail = err.Error()
		return check
	}
	if codename != "23" {
		check.Detail = fmt.Sprintf("codename %s, want 23 (Granite Ridge)", codename)
		return check
	}
	version, err := d.trimmed("sys/kernel/ryzen_smu_drv/drv_version")
	if err != nil {
		check.Detail = err.Error()
		return check
	}
	smuVersion, err := d.trimmed("sys/kernel/ryzen_smu_drv/version")
	if err != nil {
		check.Detail = err.Error()
		return check
	}
	check.OK = true
	check.Detail = fmt.Sprintf("codename 23 (Granite Ridge), driver %s, SMU %s", version, smuVersion)
	return check
}

func (d *Driver) CheckReadback() machine.Check {
	check := machine.Check{Name: "readback"}
	values := make([]string, 0, len(d.cores))
	for _, core := range d.cores {
		offset, err := d.Offset(core.Core)
		if err != nil {
			check.Detail = fmt.Sprintf("core %02d: %v", core.Core, err)
			return check
		}
		values = append(values, fmt.Sprintf("%02d:%d", core.Core, offset))
	}
	check.OK = true
	check.Detail = fmt.Sprintf("%d cores read back: %s", len(d.cores), strings.Join(values, " "))
	return check
}

func (d *Driver) CheckSlotMapping() machine.Check {
	check := machine.Check{Name: "slot_mapping"}
	if d.mappingErr != nil {
		check.Detail = d.mappingErr.Error()
		return check
	}
	check.OK = true
	check.Detail = d.mappingDetail
	return check
}

func (d *Driver) BIOSContext() (machine.BIOSContext, error) {
	if !d.validated {
		return machine.BIOSContext{}, fmt.Errorf("read BIOS context: CPU and driver not validated")
	}
	var context machine.BIOSContext
	var err error
	if context.BIOSVersion, err = d.trimmed("sys/class/dmi/id/bios_version"); err != nil {
		return context, err
	}
	vendor, err := d.trimmed("sys/class/dmi/id/board_vendor")
	if err != nil {
		return context, err
	}
	board, err := d.trimmed("sys/class/dmi/id/board_name")
	if err != nil {
		return context, err
	}
	context.Board = vendor + " " + board
	info, err := d.cpuInfo()
	if err != nil {
		return context, err
	}
	context.CPUModel = info["model name"]
	context.Microcode = info["microcode"]
	if d.mb == nil {
		return context, fmt.Errorf("read boost limit: ryzen_smu is not loaded")
	}
	response, err := d.mb.Command(0x6e, [6]uint32{})
	if err != nil {
		return context, fmt.Errorf("read boost limit: %w", err)
	}
	context.BoostLimitMHz = int(response[0])
	return context, nil
}

func (d *Driver) cpuInfo() (map[string]string, error) {
	path := filepath.Join(d.root, "proc/cpuinfo")
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	info := make(map[string]string)
	for line := range strings.SplitSeq(string(data), "\n") {
		if strings.TrimSpace(line) == "" {
			break
		}
		key, value, found := strings.Cut(line, ":")
		if found {
			info[strings.TrimSpace(key)] = strings.TrimSpace(value)
		}
	}
	return info, nil
}

func (d *Driver) trimmed(path string) (string, error) {
	full := filepath.Join(d.root, path)
	data, err := os.ReadFile(full)
	if err != nil {
		return "", fmt.Errorf("read %s: %w", full, err)
	}
	return strings.TrimSpace(string(data)), nil
}
