package smu

import (
	"encoding/binary"
	"math"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func syntheticPMTable() ([]byte, *machine.PMTable) {
	raw := make([]byte, pmTableSize)
	want := new(machine.PMTable)
	for _, block := range []struct {
		index int
		lanes *[16]float32
	}{
		{301, &want.PowerW}, {317, &want.VoltageRequestV}, {333, &want.TemperatureC},
		{381, &want.C0Pct}, {397, &want.CC1Pct}, {413, &want.CC6Pct},
	} {
		for core := range block.lanes {
			value := float32(block.index+core) / 4
			block.lanes[core] = value
			binary.LittleEndian.PutUint32(raw[(block.index+core)*4:], math.Float32bits(value))
		}
	}
	for i := 349; i <= 380; i++ {
		binary.LittleEndian.PutUint32(raw[i*4:], math.Float32bits(float32(math.NaN())))
	}
	return raw, want
}

func TestDecodePMTable(t *testing.T) {
	t.Parallel()
	raw, want := syntheticPMTable()
	if diff := cmp.Diff(want, decodePMTable(pmTableVersion, 16, raw)); diff != "" {
		t.Fatal(diff)
	}
	for _, tc := range []struct {
		name    string
		version uint64
		cores   int
		raw     []byte
	}{
		{"wrong version", 0x620105, 16, raw},
		{"wrong size", pmTableVersion, 16, append(append([]byte(nil), raw...), 0)},
		{"short read", pmTableVersion, 16, raw[:pmTableSize-1]},
		{"wrong core count", pmTableVersion, 8, raw},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := decodePMTable(tc.version, tc.cores, tc.raw); got != nil {
				t.Fatalf("unsupported table decoded: %+v", got)
			}
		})
	}
}
