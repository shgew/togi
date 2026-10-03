package session

import (
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
)

func configSnapshot(c config.Config) journal.ConfigSnapshot {
	return journal.ConfigSnapshot{
		StartOffsets:        c.StartOffsets,
		CandidateSoloLimits: c.CandidateSoloLimits,
		Durations:           journal.ConfigDurations(c.Durations),
		Evidence:            journal.ConfigEvidence(c.Evidence),
		Checking:            journal.ConfigChecking(c.Checking),
		DeadEnds:            journal.ConfigDeadEnds(c.DeadEnds),
		Backends:            journal.ConfigBackends(c.Backends),
		BackendUser:         c.BackendUser,
	}
}
