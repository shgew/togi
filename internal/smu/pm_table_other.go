//go:build !linux

package smu

import "github.com/shgew/togi/internal/machine"

func (c *PMTableReader) readPMTable() (uint64, *machine.PMTable, string) {
	return pmTableVersionUnavailable, nil, "unsupported platform"
}
