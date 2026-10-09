package tuner

import (
	"fmt"
	"os"
	"strconv"
	"strings"
)

// Throwaway research toggles for issue #493. Never merged to main.
// TOGI_EXPERIMENT is a comma-separated list such as
// "nolocated,escalate=2,oneway,margin=1,weighted,phase2=a,tol=1".
// Every toggle is off by default, leaving ruleset 10 unchanged.
type experiment struct {
	NoLocated bool   // V1: no located hunts; multi-core R7 failures go straight to voltage-targeted backoff
	Escalate  int    // V1: locate on idle cores only after this many backoffs stop fixing a load; 0 = never
	OneWay    bool   // V2: phase 1 only steps back after search and concludes on its first passed cycle
	Margin    int    // V4: counts shallower than the solo ceiling at which checking starts
	Weighted  bool   // V5: per-machine learned regime repeats in the checking cycle
	Phase2    string // V6: "" off, "a" one count per candidate per round, "b" bisection toward the ceiling
	Tol       int    // V6: a core is a phase-2 candidate only if its gap to the ceiling exceeds this
}

var exp = mustParseExperiment(os.Getenv("TOGI_EXPERIMENT"))

func mustParseExperiment(s string) experiment {
	e, err := parseExperiment(s)
	if err != nil {
		panic(err)
	}
	return e
}

func parseExperiment(s string) (experiment, error) {
	var e experiment
	for _, field := range strings.Split(s, ",") {
		field = strings.TrimSpace(field)
		if field == "" {
			continue
		}
		key, value, hasValue := strings.Cut(field, "=")
		number := func() (int, error) {
			n, err := strconv.Atoi(value)
			if !hasValue || err != nil || n < 0 {
				return 0, fmt.Errorf("TOGI_EXPERIMENT: %q needs a non-negative integer", field)
			}
			return n, nil
		}
		var err error
		switch key {
		case "nolocated":
			e.NoLocated = true
		case "escalate":
			e.Escalate, err = number()
		case "oneway":
			e.OneWay = true
		case "margin":
			e.Margin, err = number()
		case "weighted":
			e.Weighted = true
		case "phase2":
			if value != "a" && value != "b" {
				return e, fmt.Errorf("TOGI_EXPERIMENT: phase2 must be a or b, got %q", value)
			}
			e.Phase2 = value
		case "tol":
			e.Tol, err = number()
		default:
			return e, fmt.Errorf("TOGI_EXPERIMENT: unknown toggle %q", key)
		}
		if err != nil {
			return e, err
		}
	}
	if e.Phase2 != "" && !e.OneWay {
		return e, fmt.Errorf("TOGI_EXPERIMENT: phase2 needs oneway")
	}
	return e, nil
}
