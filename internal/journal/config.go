package journal

import "github.com/shgew/togi/internal/machine"

type ConfigSnapshot struct {
	StartOffsets   map[int]int     `json:"start_offsets"`
	CandidateEdges map[int]int     `json:"candidate_edges"`
	Durations      ConfigDurations `json:"durations"`
	Evidence       ConfigEvidence  `json:"evidence"`
	Guard          ConfigGuard     `json:"guard"`
	DeadEnds       ConfigDeadEnds  `json:"dead_ends"`
	Backends       ConfigBackends  `json:"backends"`
	BackendUser    string          `json:"backend_user"`
}

type ConfigDurations struct {
	SearchTrialS  int `json:"search_trial_s"`
	StartS        int `json:"start_s"`
	GuardTrialS   int `json:"guard_trial_s"`
	GuardIdleS    int `json:"guard_idle_s"`
	GuardAllCoreS int `json:"guard_all_core_s"`
}

type ConfigEvidence struct {
	Miss float64 `json:"miss"`
	Rate float64 `json:"rate"`
}

type ConfigGuard struct {
	Rotation []machine.Regime `json:"rotation"`
}

type ConfigDeadEnds struct {
	InconclusiveInARow int `json:"inconclusive_in_a_row"`
	StrayCrashesInARow int `json:"stray_crashes_in_a_row"`
}

type ConfigBackends struct {
	Mprime    string `json:"mprime"`
	Ycruncher string `json:"ycruncher"`
}
