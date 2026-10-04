package journal

import "github.com/shgew/togi/internal/machine"

type ConfigSnapshot struct {
	StartOffsets        map[int]int     `json:"start_offsets"`
	CandidateSoloLimits map[int]int     `json:"candidate_solo_limits"`
	Durations           ConfigDurations `json:"durations"`
	Evidence            ConfigEvidence  `json:"evidence"`
	Checking            ConfigChecking  `json:"checking"`
	DeadEnds            ConfigDeadEnds  `json:"dead_ends"`
	Backends            ConfigBackends  `json:"backends"`
	BackendUser         string          `json:"backend_user"`
}

type ConfigDurations struct {
	SearchTrialS     int `json:"search_trial_s"`
	ShortTrialS           int `json:"short_trial_s"`
	CheckingTrialS   int `json:"checking_trial_s"`
	CheckingIdleS    int `json:"checking_idle_s"`
	CheckingAllCoreS int `json:"checking_all_core_s"`
}

type ConfigEvidence struct {
	Miss float64 `json:"miss"`
	Rate float64 `json:"rate"`
}

type ConfigChecking struct {
	Cycle []machine.Regime `json:"cycle"`
}

type ConfigDeadEnds struct {
	InconclusiveInARow int `json:"inconclusive_in_a_row"`
	StrayCrashesInARow int `json:"stray_crashes_in_a_row"`
}

type ConfigBackends struct {
	Mprime    string `json:"mprime"`
	Ycruncher string `json:"ycruncher"`
}
