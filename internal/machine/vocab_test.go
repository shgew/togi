package machine

import (
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestBankAttributionScope(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		bank  BankType
		local bool
	}{
		{LoadStore, true}, {InstructionFetch, true}, {L2Cache, true}, {DecodeUnit, true}, {ExecutionUnit, true}, {FloatingPoint, true},
		{L3Cache, false}, {MemoryController, false}, {DataFabric, false}, {OtherBank, false}, {UnknownBank, false}, {"future-bank", false},
	} {
		t.Run(string(tc.bank), func(t *testing.T) {
			if diff := cmp.Diff(tc.local, tc.bank.CoreLocal()); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}
