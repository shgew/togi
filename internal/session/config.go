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

// ConfigFromSnapshot returns the configuration a config.loaded recorded.
func ConfigFromSnapshot(s journal.ConfigSnapshot) config.Config {
	return config.Config{
		StartOffsets:        s.StartOffsets,
		CandidateSoloLimits: s.CandidateSoloLimits,
		Durations:           config.Durations(s.Durations),
		Evidence:            config.Evidence(s.Evidence),
		Checking:            config.Checking(s.Checking),
		DeadEnds:            config.DeadEnds(s.DeadEnds),
		Backends:            config.Backends(s.Backends),
		BackendUser:         s.BackendUser,
	}
}
