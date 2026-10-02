package smu

import (
	"encoding/binary"
	"fmt"
	"path/filepath"

	"github.com/shgew/togi/internal/machine"
)

func (c *Conditions) readPMTable() (uint64, *machine.PMTable, string) {
	root := filepath.Join(c.root, "sys/kernel/ryzen_smu_drv")
	rawVersion, err := c.readFile(filepath.Join(root, "pm_table_version"))
	if err != nil {
		return pmTableVersionUnavailable, nil, "table version unreadable"
	}
	if len(rawVersion) != 4 {
		return pmTableVersionUnavailable, nil, "table version invalid"
	}
	version := uint64(binary.LittleEndian.Uint32(rawVersion))
	c.mu.Lock()
	c.seenVersion = version
	c.mu.Unlock()
	if version != pmTableVersion {
		return version, nil, "unsupported version"
	}
	if c.topologyDetail != "" {
		return version, nil, c.topologyDetail
	}
	raw, err := c.readFile(filepath.Join(root, "pm_table"))
	if err != nil {
		return version, nil, "table unreadable"
	}
	p := decodePMTable(version, c.cores, raw)
	if p == nil {
		return version, nil, fmt.Sprintf("unsupported table size %d or invalid lane values", len(raw))
	}
	return version, p, ""
}
