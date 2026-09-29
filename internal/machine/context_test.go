package machine_test

import (
	"encoding/json"
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestCompareContextAfterJournalRoundTrip(t *testing.T) {
	host := machine.BIOSContext{BIOSVersion: "F3\xff\xfe", Board: "X870E \xc3", CPUModel: "AMD Ryzen 9 9950X", Microcode: "0xb404032", BoostLimitMHz: 5700}
	raw, err := json.Marshal(&journal.SessionContext{BIOSContext: host})
	if err != nil { t.Fatal(err) }
	var recorded journal.SessionContext
	if err := json.Unmarshal(raw, &recorded); err != nil { t.Fatal(err) }
	for _, tt := range []struct {
		name string
		current machine.BIOSContext
		ok bool
	}{
		{"unchanged host", host, true},
		{"changed microcode", machine.BIOSContext{BIOSVersion: host.BIOSVersion, Board: host.Board, CPUModel: host.CPUModel, Microcode: "0xb404035", BoostLimitMHz: 5700}, false},
		{"changed invalid byte", machine.BIOSContext{BIOSVersion: "F3\xff", Board: host.Board, CPUModel: host.CPUModel, Microcode: host.Microcode, BoostLimitMHz: 5700}, false},
	} {
		if detail, ok := machine.CompareContext(recorded.BIOSContext, tt.current); ok != tt.ok {
			t.Errorf("%s: CompareContext = %q, %v; want ok %v", tt.name, detail, ok, tt.ok)
		}
	}
}
