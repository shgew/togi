//go:build !linux

package smu

import "github.com/shgew/togi/internal/machine"

func (c *Conditions) readPMTable() (uint64, *machine.PMTable, string) {
	return pmTableVersionUnavailable, nil, "unsupported platform"
}
