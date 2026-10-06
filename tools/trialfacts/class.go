package trialfacts

import (
	"encoding/json"
	"time"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/machine"
)

func ProfileClassKey(r Record) string {
	key, _ := json.Marshal(struct {
		Profile []int
		Class   facts.Class
	}{r.Profile, r.Class})
	return string(key)
}

// Spec leaves the condition unset: failure probability does not depend on it.
func Spec(class facts.Class) machine.TrialSpec {
	return machine.TrialSpec{Regime: class.Regime, Workload: machine.Workload{ID: class.Workload}, Cores: class.Cores, Duration: time.Duration(class.DurationS) * time.Second}
}
