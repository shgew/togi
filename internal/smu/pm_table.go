package smu

import (
	"encoding/binary"
	"fmt"
	"math"
	"sync"
	"time"

	"github.com/shgew/togi/internal/machine"
)

const pmTableVersion = 0x620205
const pmTableSize = 2452

const pmTableVersionUnavailable = ^uint64(0)

const pmTableReadTimeout = 100 * time.Millisecond
const pmTableMaxAge = 2 * time.Second

type pmTableResult struct {
	version uint64
	table   *machine.PMTable
	detail  string
	started time.Time
}

type Conditions struct {
	root           string
	cores          int
	topologyDetail string
	readFile       func(string) ([]byte, error)

	mu          sync.Mutex
	done        chan struct{}
	started     time.Time
	seenVersion uint64
	latest      pmTableResult
}

func NewConditions(root string, cores []machine.CoreInfo, readFile func(string) ([]byte, error)) *Conditions {
	c := &Conditions{root: root, cores: len(cores), readFile: readFile, seenVersion: pmTableVersionUnavailable}
	if len(cores) != 16 {
		c.topologyDetail = fmt.Sprintf("unsupported core count %d", len(cores))
	} else {
		for i, core := range cores {
			if core.Core != i || core.CCD != i/8 {
				c.topologyDetail = "unsupported topology: lanes require core IDs 0–15 in CCD/slot order (0–7 on CCD0, 8–15 on CCD1)"
				break
			}
		}
	}
	c.mu.Lock()
	c.startRead(time.Now())
	c.mu.Unlock()
	return c
}

// A stuck sysfs transfer cannot be cancelled; keep its one reader instead of
// launching a replacement on each timeout.
func (c *Conditions) startRead(now time.Time) {
	c.done = make(chan struct{})
	c.started = now
	go func() {
		version, table, detail := c.readPMTable()
		finished := time.Now()
		if finished.Sub(now) >= pmTableReadTimeout {
			table, detail = nil, "table read exceeded 100ms"
		}
		c.mu.Lock()
		c.latest = pmTableResult{version: version, table: table, detail: detail, started: now}
		c.seenVersion = version
		close(c.done)
		c.done = nil
		c.mu.Unlock()
	}()
}

func (c *Conditions) latestResult(now time.Time) pmTableResult {
	p := c.latest
	switch {
	case c.done != nil && now.Sub(c.started) >= pmTableReadTimeout:
		p.version, p.table, p.detail = c.seenVersion, nil, "table read overdue (100ms limit)"
	case p.started.IsZero():
		p.version, p.table, p.detail = c.seenVersion, nil, "table read pending"
	case now.Sub(p.started) > pmTableMaxAge:
		p.table, p.detail = nil, "table reading older than 2s"
	}
	return p
}

func decodePMTable(version uint64, cores int, raw []byte) *machine.PMTable {
	if version != pmTableVersion || cores != 16 || len(raw) != pmTableSize {
		return nil
	}
	p := new(machine.PMTable)
	for _, block := range []struct {
		index int
		lanes *[16]float32
	}{
		{301, &p.PowerW}, {317, &p.VoltageRequestV}, {333, &p.TemperatureC},
		{381, &p.C0Pct}, {397, &p.CC1Pct}, {413, &p.CC6Pct},
	} {
		for core := range block.lanes {
			at := (block.index + core) * 4
			value := math.Float32frombits(binary.LittleEndian.Uint32(raw[at : at+4]))
			if math.IsNaN(float64(value)) || math.IsInf(float64(value), 0) {
				return nil
			}
			block.lanes[core] = value
		}
	}
	return p
}

func (c *Conditions) PMTable() *machine.PMTable {
	c.mu.Lock()
	defer c.mu.Unlock()
	now := time.Now()
	p := c.latestResult(now)
	if c.done == nil {
		c.startRead(now)
	}
	return p.table
}

func (c *Conditions) Check() machine.Check {
	c.mu.Lock()
	now := time.Now()
	if c.done == nil {
		c.startRead(now)
	}
	done, remaining := c.done, max(0, pmTableReadTimeout-now.Sub(c.started))
	c.mu.Unlock()
	timer := time.NewTimer(remaining)
	defer timer.Stop()
	select {
	case <-done:
	case <-timer.C:
	}
	c.mu.Lock()
	p := c.latestResult(time.Now())
	c.mu.Unlock()
	version, detail := p.version, p.detail
	switch {
	case p.table != nil:
		detail = fmt.Sprintf("pm_table version 0x%x: decoding per-core power, voltage request, temperature and C-state lanes", version)
	case version != pmTableVersionUnavailable:
		detail = fmt.Sprintf("pm_table version 0x%x: per-core lanes absent (%s)", version, detail)
	default:
		detail = "pm_table version unavailable: per-core lanes absent (" + detail + ")"
	}
	return machine.Check{Name: "pm_table", Detail: detail, OK: true}
}
